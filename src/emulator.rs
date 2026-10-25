use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Presence {
    Unknown,
    Off,
    Running { serial: String },
    Offline { serial: String },
    Unauthorized { serial: String },
}

#[derive(Clone, Debug)]
pub struct Avd {
    pub name: String,
    pub presence: Presence,
    pub summary: Option<String>,
    pub detail: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Scan {
    /// `None` when the installed-AVD list itself could not be read.
    pub avds: Option<Vec<Avd>>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub enum StopTarget {
    Adb { serial: String },
    Pid { pid: u32 },
}

struct Tools {
    emulator: PathBuf,
    adb: PathBuf,
    sdk_root: Option<PathBuf>,
}

pub fn scan() -> Scan {
    let tools = match find_tools() {
        Ok(tools) => tools,
        Err(error) => {
            return Scan {
                avds: None,
                error: Some(error),
            };
        }
    };

    if let Some(error) = avd_home_error() {
        return Scan {
            avds: None,
            error: Some(error),
        };
    }

    let names = match list_avds(&tools) {
        Ok(names) => names,
        Err(error) => {
            return Scan {
                avds: None,
                error: Some(error),
            };
        }
    };

    let devices = match adb_devices(&tools) {
        Ok(devices) => devices,
        Err(error) => {
            let avds = names
                .into_iter()
                .map(|name| Avd {
                    summary: Some(
                        "Running state is unknown because adb could not be queried.".into(),
                    ),
                    detail: Some(error.clone()),
                    name,
                    presence: Presence::Unknown,
                })
                .collect();
            return Scan {
                avds: Some(avds),
                error: Some(error),
            };
        }
    };

    let mut avds: Vec<Avd> = names
        .into_iter()
        .map(|name| Avd {
            name,
            presence: Presence::Off,
            summary: None,
            detail: None,
        })
        .collect();
    let mut unmatched = Vec::new();
    let processes = running_avd_processes();

    for device in devices {
        if !device.serial.starts_with("emulator-") {
            continue;
        }
        match device.state.as_str() {
            "device" => match avd_name(&tools, &device.serial) {
                Ok(name) => assign_named(
                    &mut avds,
                    &mut unmatched,
                    name,
                    Presence::Running {
                        serial: device.serial,
                    },
                    None,
                    None,
                ),
                Err(reason) => {
                    if let Some(name) = claim_process_name(&processes, &avds) {
                        assign_named(
                            &mut avds,
                            &mut unmatched,
                            name,
                            Presence::Running {
                                serial: device.serial,
                            },
                            None,
                            None,
                        );
                    } else {
                        unmatched.push(Avd {
                            summary: Some(
                                "adb can see this emulator, but its AVD name could not be read."
                                    .into(),
                            ),
                            detail: Some(reason),
                            name: format!("Unknown ({})", device.serial),
                            presence: Presence::Running {
                                serial: device.serial,
                            },
                        });
                    }
                }
            },
            "offline" => assign_bad_state(
                &mut avds,
                &mut unmatched,
                &processes,
                device.serial,
                "offline",
            ),
            "unauthorized" | "no permissions" => assign_bad_state(
                &mut avds,
                &mut unmatched,
                &processes,
                device.serial,
                "unauthorized",
            ),
            other => unmatched.push(Avd {
                summary: Some(format!("adb reports this emulator as `{other}`.")),
                detail: Some(format!(
                    "emulator {} is in adb state `{other}`, which is not a ready device. Start and stop may fail until the state becomes `device`. Press Read emulators after the emulator finishes booting.",
                    device.serial
                )),
                name: format!("Unknown ({})", device.serial),
                presence: Presence::Unknown,
            }),
        }
    }

    avds.extend(unmatched);
    Scan {
        avds: Some(avds),
        error: None,
    }
}

pub fn start(name: &str) -> Result<u32, String> {
    let tools = find_tools()?;
    if let Some(error) = avd_home_error() {
        return Err(format!("Could not start {name}. {error}"));
    }

    let log_path = log_path(name)?;
    let exit_path = exit_path(name)?;
    let _ = fs::remove_file(&exit_path);

    let log_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .map_err(|error| {
            format!(
                "Could not start {name}. The log file {} could not be created ({error}), so the emulator was not launched. Without that file a startup failure would have no explanation.",
                log_path.display()
            )
        })?;
    let stderr = log_file.try_clone().map_err(|error| {
        format!(
            "Could not start {name}. The log file {} could not be attached to the emulator ({error}), so it was not launched.",
            log_path.display()
        )
    })?;

    let mut command = prepared(&tools.emulator, &tools);
    command
        .arg("-avd")
        .arg(name)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(stderr))
        .process_group(0);

    let mut child = command.spawn().map_err(|error| {
        format!(
            "Could not start {name}. `{} -avd {name}` failed to launch: {error}. No emulator process is running.",
            tools.emulator.display()
        )
    })?;
    let pid = child.id();
    let exit_for_waiter = exit_path.clone();
    thread::spawn(move || {
        let record = match child.wait() {
            Ok(status) => exit_label(&status),
            Err(error) => format!("wait failed: {error}"),
        };
        let _ = fs::write(&exit_for_waiter, record);
    });

    thread::sleep(Duration::from_millis(1200));
    if let Some(exit) = read_exit(&exit_path) {
        return Err(startup_failure(name, &exit, &log_path));
    }
    if !process_alive(pid) {
        let exit = read_exit(&exit_path).unwrap_or_else(|| "an unknown status".into());
        return Err(startup_failure(name, &exit, &log_path));
    }
    Ok(pid)
}

pub fn stop(name: &str, target: &StopTarget) -> Result<(), String> {
    match target {
        StopTarget::Adb { serial } => stop_with_adb(name, serial),
        StopTarget::Pid { pid } => stop_with_signal(name, *pid),
    }
}

pub fn startup_failure_if_exited(name: &str, pid: u32) -> Option<String> {
    if let Some(exit) = exit_path(name).ok().and_then(|path| read_exit(&path)) {
        let log_path = log_path(name).ok();
        return Some(died_while_starting(name, pid, &exit, log_path.as_deref()));
    }
    if !process_alive(pid) {
        let exit = exit_path(name)
            .ok()
            .and_then(|path| read_exit(&path))
            .unwrap_or_else(|| "no recorded exit status".into());
        let log_path = log_path(name).ok();
        return Some(died_while_starting(name, pid, &exit, log_path.as_deref()));
    }
    None
}

fn find_tools() -> Result<Tools, String> {
    let mut notes = Vec::new();
    match (search_path("emulator"), search_path("adb")) {
        (Ok(emulator), Ok(adb)) => {
            return Ok(Tools {
                sdk_root: infer_sdk_root(&emulator, &adb),
                emulator,
                adb,
            });
        }
        (emulator, adb) => {
            if let Err(note) = emulator {
                notes.push(format!("PATH emulator: {note}"));
            }
            if let Err(note) = adb {
                notes.push(format!("PATH adb: {note}"));
            }
        }
    }

    for (label, path) in candidate_roots() {
        match check_root(&path) {
            RootCheck::Ready { emulator, adb } => {
                return Ok(Tools {
                    emulator,
                    adb,
                    sdk_root: Some(path),
                });
            }
            RootCheck::Note(note) => notes.push(format!("{label}: {note}")),
        }
    }

    Err(missing_tools_message(&notes))
}

fn missing_tools_message(notes: &[String]) -> String {
    format!(
        "Android SDK tools were not found, so installed emulators cannot be listed or started.\n\nChecked:\n{}",
        notes.join("\n")
    )
}

fn candidate_roots() -> Vec<(String, PathBuf)> {
    let mut roots = Vec::new();
    for variable in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        match std::env::var_os(variable) {
            Some(value) if !value.is_empty() => {
                let path = PathBuf::from(value);
                if roots.iter().any(|(_, existing)| existing == &path) {
                    continue;
                }
                roots.push((variable.to_string(), path));
            }
            Some(_) => roots.push((
                variable.to_string(),
                PathBuf::from(format!("<{variable} is set but empty>")),
            )),
            None => roots.push((variable.to_string(), PathBuf::from(format!("<{variable} is unset>")))),
        }
    }

    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => {
            roots.push(("HOME/Android/Sdk".into(), PathBuf::from(home).join("Android/Sdk")));
        }
        Some(_) => roots.push((
            "HOME/Android/Sdk".into(),
            PathBuf::from("<HOME is set but empty>"),
        )),
        None => roots.push((
            "HOME/Android/Sdk".into(),
            PathBuf::from("<HOME is unset, so $HOME/Android/Sdk was not searched>"),
        )),
    }

    roots.push(("/opt/android-sdk".into(), PathBuf::from("/opt/android-sdk")));
    roots.push((
        "/usr/lib/android-sdk".into(),
        PathBuf::from("/usr/lib/android-sdk"),
    ));
    roots
}

enum RootCheck {
    Ready { emulator: PathBuf, adb: PathBuf },
    Note(String),
}

fn check_root(path: &Path) -> RootCheck {
    if path
        .to_str()
        .is_some_and(|text| text.starts_with('<') && text.ends_with('>'))
    {
        return RootCheck::Note(path.display().to_string());
    }

    match fs::metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => RootCheck::Note(format!(
            "{} does not exist.",
            path.display()
        )),
        Err(error) => RootCheck::Note(format!(
            "{} could not be read: {error}.",
            path.display()
        )),
        Ok(metadata) if !metadata.is_dir() => RootCheck::Note(format!(
            "{} exists but is not a directory, so emulator/ and platform-tools/ cannot be read.",
            path.display()
        )),
        Ok(_) => {
            let emulator = path.join("emulator/emulator");
            let adb = path.join("platform-tools/adb");
            match (file_status(&emulator), file_status(&adb)) {
                (FileStatus::Ok, FileStatus::Ok) => RootCheck::Ready { emulator, adb },
                (emulator_status, adb_status) => RootCheck::Note(format!(
                    "{} exists, but the tools are not usable. {}: {emulator_status}. {}: {adb_status}.",
                    path.display(),
                    emulator.display(),
                    adb.display()
                )),
            }
        }
    }
}

enum FileStatus {
    Ok,
    NotFound,
    NotAFile,
    NotExecutable,
    Unreadable(io::Error),
}

impl std::fmt::Display for FileStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ok => formatter.write_str("ok"),
            Self::NotFound => formatter.write_str("does not exist"),
            Self::NotAFile => formatter.write_str("exists but is not a file"),
            Self::NotExecutable => formatter.write_str("exists but is not executable"),
            Self::Unreadable(error) => write!(formatter, "could not be read: {error}"),
        }
    }
}

fn file_status(path: &Path) -> FileStatus {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return FileStatus::NotFound,
        Err(error) => return FileStatus::Unreadable(error),
    };
    if !metadata.is_file() {
        return FileStatus::NotAFile;
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        return FileStatus::NotExecutable;
    }
    FileStatus::Ok
}

fn search_path(name: &str) -> Result<PathBuf, String> {
    let Some(path_var) = std::env::var_os("PATH") else {
        return Err(format!("PATH is unset, so `{name}` could not be searched."));
    };
    if path_var.is_empty() {
        return Err(format!("PATH is empty, so `{name}` could not be searched."));
    }

    let mut unreadable = Vec::new();
    for directory in std::env::split_paths(&path_var) {
        let candidate = directory.join(name);
        match file_status(&candidate) {
            FileStatus::Ok => return Ok(candidate),
            FileStatus::NotFound => {}
            other => unreadable.push(format!("{}: {other}", candidate.display())),
        }
    }

    if unreadable.is_empty() {
        Err(format!("PATH does not contain an executable `{name}`."))
    } else {
        Err(format!(
            "PATH contains `{name}`, but it could not be used:\n{}",
            unreadable.join("\n")
        ))
    }
}

fn infer_sdk_root(emulator: &Path, adb: &Path) -> Option<PathBuf> {
    sdk_parent(emulator, "emulator").or_else(|| sdk_parent(adb, "platform-tools"))
}

fn sdk_parent(binary: &Path, directory_name: &str) -> Option<PathBuf> {
    let parent = binary.parent()?;
    if parent.file_name()?.to_str()? == directory_name {
        parent.parent().map(Path::to_path_buf)
    } else {
        None
    }
}

fn avd_home_error() -> Option<String> {
    let value = std::env::var_os("ANDROID_AVD_HOME")?;
    if value.is_empty() {
        return Some(
            "ANDROID_AVD_HOME is set but empty, so the virtual-device directory could not be read. Unset it or point it at the directory that contains your AVD files, then press Read emulators."
                .into(),
        );
    }
    let path = PathBuf::from(value);
    match fs::read_dir(&path) {
        Ok(_) => None,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Some(format!(
            "ANDROID_AVD_HOME is set to {}, but that directory does not exist. Installed emulators cannot be listed until the variable points at the folder that contains your AVD files.",
            path.display()
        )),
        Err(error) => Some(format!(
            "ANDROID_AVD_HOME is set to {}, but that directory could not be read: {error}. Installed emulators cannot be listed until the directory is readable.",
            path.display()
        )),
    }
}

fn list_avds(tools: &Tools) -> Result<Vec<String>, String> {
    let output = prepared(&tools.emulator, tools)
        .arg("-list-avds")
        .output()
        .map_err(|error| {
            format!(
                "Could not list virtual devices. `{} -list-avds` failed to run: {error}. The program was found, but the system refused to execute it, so no emulators were loaded.",
                tools.emulator.display()
            )
        })?;
    if !output.status.success() {
        let details = combined_output(&output.stdout, &output.stderr);
        let printed = if details.is_empty() {
            "The emulator printed nothing.".into()
        } else {
            format!("The emulator printed:\n{details}")
        };
        return Err(format!(
            "Could not list virtual devices. `{} -list-avds` failed with {}. No emulators were loaded.\n\n{printed}",
            tools.emulator.display(),
            exit_label(&output.status)
        ));
    }
    Ok(parse_avd_list(&String::from_utf8_lossy(&output.stdout)))
}

fn adb_devices(tools: &Tools) -> Result<Vec<AdbDevice>, String> {
    let output = prepared(&tools.adb, tools)
        .args(["devices", "-l"])
        .output()
        .map_err(|error| {
            format!(
                "Could not check which emulators are running. `{} devices -l` failed to run: {error}. Running status was not updated.",
                tools.adb.display()
            )
        })?;
    if !output.status.success() {
        let details = combined_output(&output.stdout, &output.stderr);
        let printed = if details.is_empty() {
            "adb printed nothing.".into()
        } else {
            format!("adb said:\n{details}")
        };
        return Err(format!(
            "Could not check which emulators are running. `{} devices -l` failed with {}. Running status was not updated.\n\n{printed}",
            tools.adb.display(),
            exit_label(&output.status)
        ));
    }
    Ok(parse_adb_devices(&String::from_utf8_lossy(&output.stdout)))
}

fn avd_name(tools: &Tools, serial: &str) -> Result<String, String> {
    let output = prepared(&tools.adb, tools)
        .args(["-s", serial, "emu", "avd", "name"])
        .output()
        .map_err(|error| {
            format!(
                "An emulator is running as {serial}, but its AVD name could not be read. `adb -s {serial} emu avd name` failed to run: {error}. It is shown as an unknown device and can still be stopped."
            )
        })?;
    let text = combined_output(&output.stdout, &output.stderr);
    if !output.status.success() {
        let printed = if text.is_empty() {
            "adb printed nothing.".into()
        } else {
            format!("adb said:\n{text}")
        };
        return Err(format!(
            "An emulator is running as {serial}, but its AVD name could not be read. `adb -s {serial} emu avd name` failed with {}. It is shown as an unknown device and can still be stopped.\n\n{printed}",
            exit_label(&output.status)
        ));
    }
    parse_avd_name(&text).ok_or_else(|| {
        format!(
            "An emulator is running as {serial}, but `adb emu avd name` did not print an AVD name. Output was:\n{text}\nIt is shown as an unknown device and can still be stopped."
        )
    })
}

fn prepared(program: &Path, tools: &Tools) -> Command {
    let mut command = Command::new(program);
    if let Some(root) = &tools.sdk_root {
        command.env("ANDROID_HOME", root);
        command.env("ANDROID_SDK_ROOT", root);
    }
    command.env("PATH", extended_path(tools));
    command
}

fn extended_path(tools: &Tools) -> std::ffi::OsString {
    let mut path = std::ffi::OsString::new();
    let mut push_dir = |directory: &Path| {
        if !path.is_empty() {
            path.push(":");
        }
        path.push(directory);
    };
    if let Some(root) = &tools.sdk_root {
        push_dir(&root.join("platform-tools"));
        push_dir(&root.join("emulator"));
    }
    if let Some(directory) = tools.emulator.parent() {
        push_dir(directory);
    }
    if let Some(directory) = tools.adb.parent() {
        push_dir(directory);
    }
    if let Some(current) = std::env::var_os("PATH") {
        if !path.is_empty() {
            path.push(":");
        }
        path.push(current);
    }
    path
}

fn stop_with_adb(name: &str, serial: &str) -> Result<(), String> {
    let tools = find_tools().map_err(|error| {
        format!("Could not stop {name} ({serial}). {error} The emulator is still running.")
    })?;
    let output = prepared(&tools.adb, &tools)
        .args(["-s", serial, "emu", "kill"])
        .output()
        .map_err(|error| {
            format!(
                "Could not stop {name} ({serial}). Failed to run `{} -s {serial} emu kill`: {error}. The emulator is still running.",
                tools.adb.display()
            )
        })?;
    let text = combined_output(&output.stdout, &output.stderr);
    let failed = !output.status.success()
        || text.to_ascii_lowercase().contains("error")
        || text.starts_with("KO")
        || text.contains("\nKO");
    if failed {
        let printed = if text.is_empty() {
            "adb printed nothing. The emulator console is not accepting commands, often because the device is offline, still booting, or adb lost the connection.".into()
        } else {
            format!("adb said:\n{text}")
        };
        return Err(format!(
            "Could not stop {name} ({serial}). `adb -s {serial} emu kill` failed with {}. The emulator is still running.\n\n{printed}",
            exit_label(&output.status)
        ));
    }
    Ok(())
}

fn stop_with_signal(name: &str, pid: u32) -> Result<(), String> {
    if !process_alive(pid) {
        return Ok(());
    }
    let status = Command::new("kill")
        .args(["-TERM", &format!("-{pid}")])
        .status()
        .map_err(|error| {
            format!(
                "Could not stop {name}. It is still starting (pid {pid}) and adb has no serial yet, so `emu kill` cannot be used. `kill -TERM -{pid}` failed to run: {error}. The emulator is still running."
            )
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "Could not stop {name}. It is still starting (pid {pid}) and has no adb serial, so `emu kill` was not sent. `kill -TERM -{pid}` failed with {}. The emulator process is still running.",
            exit_label(&status)
        ))
    }
}

fn assign_named(
    avds: &mut [Avd],
    unmatched: &mut Vec<Avd>,
    name: String,
    presence: Presence,
    summary: Option<String>,
    detail: Option<String>,
) {
    if let Some(avd) = avds.iter_mut().find(|avd| avd.name == name) {
        avd.presence = presence;
        avd.summary = summary;
        avd.detail = detail;
        return;
    }
    let serial = presence_serial(&presence).unwrap_or("unknown");
    unmatched.push(Avd {
        summary: summary.or_else(|| {
            Some("This running emulator is not in the installed AVD list.".into())
        }),
        detail: detail.or_else(|| {
            Some(format!(
                "{name} is running as {serial}, but `emulator -list-avds` did not list that name. It can still be stopped. If it should appear as installed, check the AVD directory and press Read emulators again."
            ))
        }),
        name,
        presence,
    });
}

fn assign_bad_state(
    avds: &mut [Avd],
    unmatched: &mut Vec<Avd>,
    processes: &[(String, u32)],
    serial: String,
    kind: &str,
) {
    let (summary, detail) = bad_state_text(&serial, kind);
    let presence = match kind {
        "offline" => Presence::Offline {
            serial: serial.clone(),
        },
        _ => Presence::Unauthorized {
            serial: serial.clone(),
        },
    };
    if let Some(name) = claim_process_name(processes, avds) {
        assign_named(
            avds,
            unmatched,
            name,
            presence,
            Some(summary),
            Some(detail),
        );
        return;
    }
    unmatched.push(Avd {
        name: format!("Unknown ({serial})"),
        presence,
        summary: Some(summary),
        detail: Some(detail),
    });
}

fn claim_process_name(processes: &[(String, u32)], avds: &[Avd]) -> Option<String> {
    let mut open: Vec<&str> = processes
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| {
            avds.iter().any(|avd| {
                avd.name == *name && matches!(avd.presence, Presence::Off | Presence::Unknown)
            })
        })
        .collect();
    open.sort_unstable();
    open.dedup();
    (open.len() == 1).then(|| open[0].to_string())
}

fn presence_serial(presence: &Presence) -> Option<&str> {
    match presence {
        Presence::Running { serial }
        | Presence::Offline { serial }
        | Presence::Unauthorized { serial } => Some(serial),
        Presence::Unknown | Presence::Off => None,
    }
}

fn running_avd_processes() -> Vec<(String, u32)> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(bytes) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<String> = bytes
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect();
        let looks_like_emulator = args
            .iter()
            .any(|arg| arg.contains("emulator") || arg.contains("qemu"));
        if !looks_like_emulator {
            continue;
        }
        if let Some(name) = args
            .windows(2)
            .find_map(|pair| (pair[0] == "-avd").then(|| pair[1].clone()))
        {
            found.push((name, pid));
        }
    }
    found
}

fn bad_state_text(serial: &str, kind: &str) -> (String, String) {
    match kind {
        "offline" => (
            format!("{serial} is offline, so it is not accepting commands."),
            format!(
                "{serial} is offline. adb can see the emulator, but the device is not accepting commands. It is usually still booting. If it stays offline, startup failed inside the emulator (snapshot, disk, or graphics) and Start will not produce a usable device. Press Stop, then start it again. If Stop fails, the console is down because the device never finished connecting to adb."
            ),
        ),
        _ => (
            format!("{serial} is unauthorized, so adb cannot control it."),
            format!(
                "{serial} is unauthorized. The emulator is running, but adb is not allowed to talk to it. Open the emulator window and accept the debugging prompt. Until that prompt is accepted, Stop via `adb emu kill` is rejected and the device cannot be controlled from this applet."
            ),
        ),
    }
}

fn startup_failure(name: &str, exit: &str, log_path: &Path) -> String {
    format!(
        "Could not start {name}. The emulator process exited immediately with {exit}, before adb could see a device. Nothing is running.\n\n{}\n\nFull log: {}",
        explain_log(&read_log(log_path)),
        log_path.display()
    )
}

fn died_while_starting(name: &str, pid: u32, exit: &str, log_path: Option<&Path>) -> String {
    let explanation = log_path
        .map(|path| format!("{}\n\nFull log: {}", explain_log(&read_log(path)), path.display()))
        .unwrap_or_else(|| {
            "No log file was available, so the emulator's own reason could not be read.".into()
        });
    format!(
        "{name} stopped on its own while it was still starting (pid {pid}, {exit}). adb will not show a running device.\n\n{explanation}"
    )
}

fn explain_log(log: &str) -> String {
    let trimmed = log.trim();
    if trimmed.is_empty() {
        return "The emulator wrote no log. It usually exits this way when the AVD name does not exist, the emulator cannot load its libraries, or hardware acceleration (KVM) is unavailable.".into();
    }

    let interesting: Vec<&str> = trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("error")
                || lower.contains("fatal")
                || lower.contains("panic")
                || lower.contains("fail")
                || lower.contains("kvm")
                || lower.contains("permission denied")
        })
        .collect();

    let body = if interesting.is_empty() {
        tail_lines(trimmed, 12)
    } else {
        let start = interesting.len().saturating_sub(8);
        interesting[start..].join("\n")
    };

    let mut message = format!("This is why it stopped:\n{body}");
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("hardware acceleration") || lower.contains("kvm") {
        message.push_str(
            "\n\nHardware acceleration is required. Check that /dev/kvm exists and that your user can open it (`kvm` group).",
        );
    }
    message
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AdbDevice {
    serial: String,
    state: String,
}

fn parse_avd_list(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_adb_devices(stdout: &str) -> Vec<AdbDevice> {
    stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with("List of devices") || line.starts_with('*') {
                return None;
            }
            let mut parts = line.split_whitespace();
            let serial = parts.next()?.to_string();
            let state = parts.next()?.to_string();
            Some(AdbDevice { serial, state })
        })
        .collect()
}

fn parse_avd_name(text: &str) -> Option<String> {
    text.lines().map(str::trim).find(|line| {
        !line.is_empty() && *line != "OK" && !line.starts_with("KO") && !line.starts_with("error:")
    }).map(str::to_string)
}

fn combined_output(stdout: &[u8], stderr: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(stdout).trim().to_string();
    let err = String::from_utf8_lossy(stderr);
    let err = err.trim();
    if !err.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(err);
    }
    text
}

fn exit_label(status: &ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "a signal".into(),
    }
}

fn cache_dir() -> Result<PathBuf, String> {
    let dir = if let Some(cache) = std::env::var_os("XDG_CACHE_HOME") {
        if cache.is_empty() {
            return Err(
                "XDG_CACHE_HOME is set but empty, so the emulator log directory could not be created. The emulator was not started, because a failure would have no log to explain it."
                    .into(),
            );
        }
        PathBuf::from(cache).join("cosmic-applet-emulator")
    } else if let Some(home) = std::env::var_os("HOME") {
        if home.is_empty() {
            return Err(
                "HOME is set but empty, and XDG_CACHE_HOME is unset, so the emulator log directory could not be created. The emulator was not started."
                    .into(),
            );
        }
        PathBuf::from(home).join(".cache/cosmic-applet-emulator")
    } else {
        return Err(
            "Could not choose a log directory because both XDG_CACHE_HOME and HOME are unset. The emulator was not started, because a failure would have no log to explain it."
                .into(),
        );
    };
    fs::create_dir_all(&dir).map_err(|error| {
        format!(
            "Could not create {}: {error}. The emulator was not started, because a failure would have no log to explain it.",
            dir.display()
        )
    })?;
    Ok(dir)
}

fn safe_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn log_path(name: &str) -> Result<PathBuf, String> {
    Ok(cache_dir()?.join(format!("{}.log", safe_name(name))))
}

fn exit_path(name: &str) -> Result<PathBuf, String> {
    Ok(cache_dir()?.join(format!("{}.exit", safe_name(name))))
}

fn read_exit(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn read_log(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn tail_lines(text: &str, count: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect();
    let start = lines.len().saturating_sub(count);
    lines[start..].join("\n")
}

pub fn process_alive(pid: u32) -> bool {
    let Ok(text) = fs::read_to_string(format!("/proc/{pid}/status")) else {
        return false;
    };
    text.lines()
        .find_map(|line| line.strip_prefix("State:"))
        .map(|state| {
            let state = state.trim();
            !state.starts_with('Z') && !state.starts_with('X')
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_avd_names() {
        let names = parse_avd_list("Medium_Phone_API_37.0\n\nPixel_8\n");
        assert_eq!(
            names,
            vec![
                "Medium_Phone_API_37.0".to_string(),
                "Pixel_8".to_string()
            ]
        );
    }

    #[test]
    fn parses_adb_emulator_rows() {
        let stdout = "\
List of devices attached\n\
emulator-5554          device product:sdk model:sdk device:emu64xa\n\
emulator-5556          offline\n\
\n";
        let devices = parse_adb_devices(stdout);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].serial, "emulator-5554");
        assert_eq!(devices[0].state, "device");
        assert_eq!(devices[1].state, "offline");
    }

    #[test]
    fn parses_emu_avd_name() {
        assert_eq!(
            parse_avd_name("Medium_Phone_API_37.0\nOK\n").as_deref(),
            Some("Medium_Phone_API_37.0")
        );
    }

    #[test]
    fn explain_log_quotes_the_emulator_error() {
        let message = explain_log(
            "INFO | boot\nERROR | x86_64 emulation currently requires hardware acceleration!\n",
        );
        assert!(message.contains("hardware acceleration"));
        assert!(message.contains("This is why it stopped"));
        assert!(message.contains("/dev/kvm"));
    }

    #[test]
    fn missing_tools_message_lists_every_checked_place() {
        let message = missing_tools_message(&[
            "PATH emulator: PATH does not contain an executable `emulator`.".into(),
            "ANDROID_HOME: <ANDROID_HOME is unset>".into(),
            "/opt/android-sdk: /opt/android-sdk does not exist.".into(),
        ]);
        assert!(message.contains("cannot be listed or started"));
        assert!(message.contains("PATH"));
        assert!(message.contains("ANDROID_HOME"));
        assert!(message.contains("/opt/android-sdk"));
    }

    #[test]
    fn missing_directory_explains_that_it_does_not_exist() {
        let missing = std::env::temp_dir().join("cosmic-applet-emulator-missing-sdk");
        let _ = fs::remove_dir_all(&missing);
        match check_root(&missing) {
            RootCheck::Note(note) => assert!(note.contains("does not exist"), "{note}"),
            RootCheck::Ready { .. } => panic!("missing directory was treated as an SDK"),
        }
    }

    #[test]
    fn unreadable_file_is_described() {
        let directory = std::env::temp_dir().join("cosmic-applet-emulator-not-a-directory");
        let _ = fs::remove_file(&directory);
        fs::write(&directory, b"not a directory").unwrap();
        match check_root(&directory) {
            RootCheck::Note(note) => {
                assert!(note.contains("not a directory"), "{note}");
            }
            RootCheck::Ready { .. } => panic!("a file was treated as an SDK"),
        }
        let _ = fs::remove_file(&directory);
    }

    #[test]
    fn scan_lists_devices_or_explains_why_it_cannot() {
        let scan = scan();
        match (&scan.avds, &scan.error) {
            (None, Some(error)) => {
                assert!(
                    error.contains("could not")
                        || error.contains("cannot")
                        || error.contains("does not exist")
                        || error.contains("Checked:")
                        || error.contains("failed"),
                    "{error}"
                );
            }
            (Some(_), None) => {}
            (Some(_), Some(error)) => {
                assert!(
                    error.contains("adb") || error.contains("running") || error.contains("could not"),
                    "{error}"
                );
            }
            (None, None) => panic!("scan returned neither a device list nor an explanation"),
        }
    }

    #[test]
    fn strips_baked_svg_color() {
        let cleaned = crate::icons::strip_inline_color(
            br#"<svg style="color: rgb(74, 85, 101); opacity: 1;" viewBox="0 0 24 24"></svg>"#,
        );
        let text = String::from_utf8(cleaned).unwrap();
        assert!(!text.contains("rgb("));
        assert!(text.contains("viewBox"));
    }
}
