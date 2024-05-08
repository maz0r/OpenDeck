pub mod info_param;
pub mod manifest;
mod webserver;

use crate::shared::{convert_icon, Action, CATEGORIES};
use crate::APP_HANDLE;

use std::process::{Command, Stdio};
use std::{fs, path};

use tauri::AppHandle;

use futures_util::StreamExt;
use tokio::net::{TcpListener, TcpStream};

use anyhow::{anyhow, Context};
use log::{error, warn};

/// Initialise a plugin from a given directory.
async fn initialise_plugin(path: &path::PathBuf) -> anyhow::Result<()> {
	let plugin_uuid = path.file_name().unwrap().to_str().unwrap();
	let manifest_path = path.join("manifest.json");

	let manifest = fs::read(&manifest_path).context("Failed to read manifest")?;
	let mut manifest: manifest::PluginManifest = serde_json::from_slice(&manifest).context("Failed to parse manifest")?;

	for action in &mut manifest.actions {
		plugin_uuid.clone_into(&mut action.plugin);

		let action_icon_path = path.join(action.icon.clone());
		action.icon = convert_icon(action_icon_path.to_str().unwrap().to_owned());

		if !action.property_inspector.is_empty() {
			action.property_inspector = path.join(&action.property_inspector).to_string_lossy().to_string();
		} else if let Some(ref property_inspector) = manifest.property_inspector_path {
			action.property_inspector = path.join(property_inspector).to_string_lossy().to_string();
		}

		for state in &mut action.states {
			if state.image == "actionDefaultImage" {
				state.image.clone_from(&action.icon);
			} else {
				let state_icon = path.join(state.image.clone());
				state.image = convert_icon(state_icon.to_str().unwrap().to_owned());
			}
		}
	}

	let mut categories = CATEGORIES.lock().await;
	if let Some(category) = categories.get_mut(&manifest.category) {
		for action in manifest.actions {
			category.push(action);
		}
	} else {
		let mut category: Vec<Action> = vec![];
		for action in manifest.actions {
			category.push(action);
		}
		categories.insert(manifest.category, category);
	}

	#[cfg(target_os = "windows")]
	let platform = "windows";
	#[cfg(target_os = "macos")]
	let platform = "mac";
	#[cfg(target_os = "linux")]
	let platform = "linux";

	let mut code_path = manifest.code_path;
	let mut use_wine = false;
	let mut supported = false;

	// Determine the method used to run the plugin based on its supported operating systems and the current operating system.
	for os in manifest.os {
		if os.platform == platform {
			#[cfg(target_os = "windows")]
			if manifest.code_path_windows.is_some() {
				code_path = manifest.code_path_windows.clone();
			}
			#[cfg(target_os = "macos")]
			if manifest.code_path_macos.is_some() {
				code_path = manifest.code_path_macos;
			}
			#[cfg(target_os = "linux")]
			if manifest.code_path_linux.is_some() {
				code_path = manifest.code_path_linux;
			}

			use_wine = false;

			supported = true;
			break;
		} else if os.platform == "windows" {
			use_wine = true;
			supported = true;
		}
	}

	if code_path.is_none() && use_wine {
		code_path = manifest.code_path_windows;
	}

	if !supported || code_path.is_none() {
		return Err(anyhow!("Unsupported on platform {}", platform));
	}

	let mut devices: Vec<info_param::DeviceInfo> = vec![];
	for device in crate::devices::DEVICES.lock().await.values() {
		devices.push(device.into());
	}

	let info = info_param::make_info(plugin_uuid.to_owned(), manifest.version).await;

	let code_path = code_path.unwrap();

	if code_path.ends_with(".html") {
		// Create a webview window for the plugin and call its registration function.
		let url = String::from("http://localhost:57118") + path.join(code_path).to_str().unwrap();
		let window = tauri::WindowBuilder::new(APP_HANDLE.get().unwrap(), plugin_uuid.replace('.', "_"), tauri::WindowUrl::External(url.parse()?))
			.visible(false)
			.build()?;

		#[cfg(debug_assertions)]
		window.open_devtools();

		window.eval(&format!(
			"const opendeckInit = () => {{
				try {{
					connectElgatoStreamDeckSocket({}, \"{}\", \"{}\", `{}`);
				}} catch (e) {{
					setTimeout(opendeckInit, 10);
				}}
			}};
			opendeckInit();
			",
			57116,
			plugin_uuid,
			"registerPlugin",
			serde_json::to_string(&info)?
		))?;
	} else if use_wine {
		if Command::new("wine").stdout(Stdio::null()).stderr(Stdio::null()).spawn().is_err() {
			return Err(anyhow!("Failed to detect an installation of Wine to run plugin {}", plugin_uuid));
		}

		// Start Wine with the appropriate arguments.
		Command::new("wine")
			.current_dir(path)
			.args([
				code_path,
				String::from("-port"),
				57116.to_string(),
				String::from("-pluginUUID"),
				plugin_uuid.to_owned(),
				String::from("-registerEvent"),
				String::from("registerPlugin"),
				String::from("-info"),
				serde_json::to_string(&info)?,
			])
			.stdout(Stdio::null())
			.stderr(Stdio::null())
			.spawn()?;
	} else {
		// Run the plugin's executable natively.
		Command::new(path.join(code_path))
			.current_dir(path)
			.args([
				String::from("-port"),
				57116.to_string(),
				String::from("-pluginUUID"),
				plugin_uuid.to_owned(),
				String::from("-registerEvent"),
				String::from("registerPlugin"),
				String::from("-info"),
				serde_json::to_string(&info)?,
			])
			.stdout(Stdio::null())
			.stderr(Stdio::null())
			.spawn()?;
	}

	Ok(())
}

/// Initialise plugins from the plugins directory.
pub fn initialise_plugins(app: AppHandle) {
	tokio::spawn(init_websocket_server());
	tokio::spawn(webserver::init_webserver(app.path_resolver().app_config_dir().unwrap()));

	let plugin_dir = app.path_resolver().app_config_dir().unwrap().join("plugins");
	let _ = fs::create_dir_all(&plugin_dir);

	if let Ok(contents) = fs::read_to_string(plugin_dir.join("removed.txt")) {
		let _ = fs::remove_dir_all(plugin_dir.join(contents));
		let _ = fs::remove_file(plugin_dir.join("removed.txt"));
	}

	let entries = match fs::read_dir(&plugin_dir) {
		Ok(p) => p,
		Err(error) => {
			error!("Failed to read plugins directory at {}: {}", plugin_dir.display(), error);
			panic!()
		}
	};

	// Iterate through all directory entries in the plugins folder and initialise them as plugins if appropriate
	for entry in entries {
		if let Ok(entry) = entry {
			let path = match entry.metadata().unwrap().is_symlink() {
				true => fs::read_link(entry.path()).unwrap(),
				false => entry.path(),
			};
			let metadata = fs::metadata(&path).unwrap();
			if metadata.is_dir() {
				tokio::spawn(async move {
					if let Err(error) = initialise_plugin(&path).await {
						warn!("Failed to initialise plugin at {}: {}", path.display(), error);
					}
				});
			} else {
				warn!("Failed to initialise plugin at {}: is a file", entry.path().display());
			}
		} else if let Err(error) = entry {
			warn!("Failed to read entry of plugins directory: {}", error)
		}
	}
}

/// Start the WebSocket server that plugins communicate with.
async fn init_websocket_server() {
	let listener = match TcpListener::bind("0.0.0.0:57116").await {
		Ok(listener) => listener,
		Err(error) => {
			error!("Failed to bind plugin WebSocket server to socket: {}", error);
			return;
		}
	};

	while let Ok((stream, _)) = listener.accept().await {
		accept_connection(stream).await;
	}
}

/// Handle incoming data from a WebSocket connection.
async fn accept_connection(stream: TcpStream) {
	let mut socket = match tokio_tungstenite::accept_async(stream).await {
		Ok(socket) => socket,
		Err(error) => {
			warn!("Failed to complete WebSocket handshake: {}", error);
			return;
		}
	};

	let Ok(register_event) = socket.next().await.unwrap() else {
		return;
	};
	match serde_json::from_str(&register_event.clone().into_text().unwrap()) {
		Ok(event) => crate::events::register_plugin(event, socket).await,
		Err(_) => {
			let _ = crate::events::inbound::process_incoming_message(Ok(register_event)).await;
		}
	}
}
