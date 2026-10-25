mod emulator;
mod icons;

use std::time::Duration;

use cosmic::app::{Core, Task};
use cosmic::iced::core::window;
use cosmic::iced::window::Id;
use cosmic::iced::{Alignment, Length, Rectangle, Subscription};
use cosmic::surface::action::{app_popup, destroy_popup};
use cosmic::widget::{self, button};
use cosmic::{Application, Element};

use emulator::{Presence, Scan, StopTarget};

const ID: &str = "io.github.emulatorstarter.CosmicApplet";

pub struct EmulatorApplet {
    core: Core,
    popup: Option<Id>,
    scan_pending: bool,
    busy: bool,
    loaded: bool,
    selected: Option<String>,
    devices: Vec<Device>,
    error: Option<String>,
    dismissed: Option<String>,
}

struct Device {
    name: String,
    presence: Presence,
    phase: Phase,
    summary: Option<String>,
    detail: Option<String>,
    pid: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Launching,
    Starting,
    Stopping,
    Failed,
}

#[derive(Clone, Debug)]
pub enum Message {
    Surface(cosmic::surface::Action<Message>),
    PopupClosed(Id),
    KickScan,
    Refresh,
    Select(String),
    Start(String),
    Stop(String),
    Scanned { manual: bool, scan: Scan },
    Started { name: String, result: Result<u32, String> },
    Stopped { name: String, result: Result<(), String> },
    DismissError,
}

impl Application for EmulatorApplet {
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = ID;

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: Self::Flags) -> (Self, Task<Message>) {
        (
            Self {
                core,
                popup: None,
                scan_pending: false,
                busy: false,
                loaded: false,
                selected: None,
                devices: Vec::new(),
                error: None,
                dismissed: None,
            },
            Task::none(),
        )
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::PopupClosed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                    self.scan_pending = false;
                }
            }
            Message::Surface(action) => {
                return cosmic::task::message(cosmic::Action::Surface(action));
            }
            Message::KickScan => {
                if self.busy {
                    return Task::none();
                }
                self.scan_pending = false;
                self.busy = true;
                return scan_task(true);
            }
            Message::Refresh => {
                if self.busy {
                    self.report(
                        "Already reading installed emulators. Wait for this check to finish before starting another.".into(),
                    );
                    return Task::none();
                }
                self.busy = true;
                self.scan_pending = false;
                return scan_task(true);
            }
            Message::Select(name) => {
                self.selected = Some(name.clone());
                if let Some(detail) = self
                    .devices
                    .iter()
                    .find(|device| device.name == name)
                    .and_then(|device| device.detail.clone())
                {
                    self.report(detail);
                }
            }
            Message::Start(name) => return self.start(name),
            Message::Stop(name) => return self.stop(name),
            Message::Scanned { manual, scan } => self.apply_scan(scan, manual),
            Message::Started { name, result } => self.started(name, result),
            Message::Stopped { name, result } => self.stopped(name, result),
            Message::DismissError => {
                self.dismissed = self.error.clone();
                self.error = None;
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let have_popup = self.popup;
        let button = self
            .core
            .applet
            .icon_button_from_handle(icons::android())
            .on_press_with_rectangle(move |offset, bounds| {
                if let Some(id) = have_popup {
                    Message::Surface(destroy_popup(id))
                } else {
                    Message::Surface(app_popup::<Self>(
                        |_| Default::default(),
                        move |state: &mut Self| {
                            let new_id = Id::unique();
                            state.popup = Some(new_id);
                            state.scan_pending = true;
                            let mut popup_settings = state.core.applet.get_popup_settings(
                                state.core.main_window_id().unwrap(),
                                new_id,
                                None,
                                None,
                                None,
                            );
                            popup_settings.positioner.anchor_rect = Rectangle {
                                x: (bounds.x - offset.x) as i32,
                                y: (bounds.y - offset.y) as i32,
                                width: bounds.width as i32,
                                height: bounds.height as i32,
                            };
                            popup_settings
                        },
                        Some(Box::new(|state: &Self| {
                            Element::from(state.core.applet.popup_container(state.popup_view()))
                                .map(cosmic::Action::App)
                        })),
                    ))
                }
            });

        Element::from(self.core.applet.applet_tooltip::<Message>(
            button,
            "Emulators",
            self.popup.is_some(),
            Message::Surface,
            None,
        ))
    }

    fn view_window(&self, _id: Id) -> Element<'_, Message> {
        widget::text("").into()
    }

    fn subscription(&self) -> Subscription<Message> {
        if self.scan_pending {
            cosmic::iced::time::every(Duration::from_millis(16)).map(|_| Message::KickScan)
        } else {
            Subscription::none()
        }
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

impl EmulatorApplet {
    fn popup_view(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut content = widget::Column::with_capacity(4)
            .spacing(spacing.space_s)
            .padding([spacing.space_s, spacing.space_s])
            .width(Length::Fill);

        let read = button::standard(if self.busy {
            "Reading…"
        } else {
            "Read emulators"
        })
        .on_press_maybe((!self.busy).then_some(Message::Refresh));

        content = content.push(
            widget::row![
                widget::text::title4("Emulators"),
                widget::space::horizontal(),
                read
            ]
            .align_y(Alignment::Center)
            .spacing(spacing.space_s)
            .width(Length::Fill),
        );

        if self.devices.is_empty() {
            let message = if self.busy || self.scan_pending {
                "Reading installed emulators…"
            } else if self.error.is_some() {
                "Installed emulators could not be read."
            } else if self.loaded {
                "No emulators are installed. Create a virtual device in Android Studio's Device Manager, then press Read emulators. There is nothing to start until an AVD exists."
            } else {
                "Press Read emulators to load every virtual device installed on this machine."
            };
            content = content.push(widget::text::body(message).width(Length::Fill));
        } else {
            let radius = cosmic::theme::active().cosmic().radius_s();
            let mut list = widget::list_column();
            for device in &self.devices {
                list = list.add(device_row(
                    device,
                    self.selected.as_deref(),
                    radius,
                    spacing.space_xs,
                ));
            }
            content = content.push(list);
        }

        if let Some(error) = &self.error {
            content = content.push(widget::warning(error.as_str()).on_close(Message::DismissError));
        }

        widget::scrollable(content).width(Length::Fill).into()
    }

    fn start(&mut self, name: String) -> Task<Message> {
        self.selected = Some(name.clone());
        let decision = self
            .devices
            .iter()
            .find(|device| device.name == name)
            .map(|device| {
                if can_start(device) {
                    Ok(())
                } else {
                    Err(already_running_reason(device))
                }
            });
        match decision {
            None => {
                self.report(format!(
                    "Could not start {name}. It is not in the list that was last read, so there is no AVD to launch. Press Read emulators and choose it again."
                ));
                return Task::none();
            }
            Some(Err(reason)) => {
                self.report(reason);
                return Task::none();
            }
            Some(Ok(())) => {}
        }

        if let Some(device) = self.devices.iter_mut().find(|device| device.name == name) {
            device.phase = Phase::Launching;
            device.summary = Some("Starting the emulator process.".into());
            device.detail = None;
        }
        self.dismissed = None;
        cosmic::task::future(async move {
            let result = emulator::start(&name);
            Message::Started { name, result }
        })
    }

    fn stop(&mut self, name: String) -> Task<Message> {
        self.selected = Some(name.clone());
        let decision = self
            .devices
            .iter()
            .find(|device| device.name == name)
            .map(|device| {
                if device.phase == Phase::Stopping {
                    Err(format!(
                        "{name} is already being stopped. Wait for it to exit before sending another stop command."
                    ))
                } else {
                    stop_target(device).ok_or_else(|| {
                        format!(
                            "Could not stop {name}. It does not have an adb serial or a process id yet, so there is nothing to signal. If it is still launching, wait a second and press Stop again."
                        )
                    })
                }
            });
        let target = match decision {
            None => {
                self.report(format!(
                    "Could not stop {name}. It is not in the current list, so there is no device to shut down. Press Read emulators and try again."
                ));
                return Task::none();
            }
            Some(Err(reason)) => {
                self.report(reason);
                return Task::none();
            }
            Some(Ok(target)) => target,
        };

        if let Some(device) = self.devices.iter_mut().find(|device| device.name == name) {
            device.phase = Phase::Stopping;
            device.summary = Some("Asking the emulator to shut down.".into());
        }
        self.dismissed = None;
        cosmic::task::future(async move {
            let result = emulator::stop(&name, &target);
            Message::Stopped { name, result }
        })
    }

    fn started(&mut self, name: String, result: Result<u32, String>) {
        let failure = if let Some(device) = self.devices.iter_mut().find(|device| device.name == name)
        {
            match result {
                Ok(pid) => {
                    device.phase = Phase::Starting;
                    device.pid = Some(pid);
                    device.summary = Some(format!(
                        "Emulator process {pid} is running. Waiting for adb to list the device. If it exits, the log explains why."
                    ));
                    device.detail = None;
                    None
                }
                Err(error) => {
                    device.phase = Phase::Failed;
                    device.pid = None;
                    device.presence = Presence::Off;
                    device.summary = Some("The emulator did not stay running.".into());
                    device.detail = Some(error.clone());
                    Some(error)
                }
            }
        } else {
            result.err()
        };
        if let Some(error) = failure {
            self.report(error);
        }
    }

    fn stopped(&mut self, name: String, result: Result<(), String>) {
        let failure = if let Some(device) = self.devices.iter_mut().find(|device| device.name == name)
        {
            match result {
                Ok(()) => {
                    device.phase = Phase::Idle;
                    device.presence = Presence::Off;
                    device.pid = None;
                    device.summary = None;
                    device.detail = None;
                    None
                }
                Err(error) => {
                    device.phase = Phase::Idle;
                    device.summary = Some("Stop failed. The emulator is still running.".into());
                    device.detail = Some(error.clone());
                    Some(error)
                }
            }
        } else {
            result.err()
        };
        if let Some(error) = failure {
            self.report(error);
        }
    }

    fn apply_scan(&mut self, scan: Scan, manual: bool) {
        self.busy = false;
        self.loaded = true;

        if let Some(error) = scan.error.clone() {
            self.report(error);
        } else if manual {
            self.error = None;
            self.dismissed = None;
        }

        let Some(found) = scan.avds else {
            if manual {
                self.devices.clear();
            }
            return;
        };

        let previous = std::mem::take(&mut self.devices);
        let mut devices = Vec::with_capacity(found.len());
        for avd in found {
            let mut device = Device {
                name: avd.name,
                presence: avd.presence,
                phase: Phase::Idle,
                summary: avd.summary,
                detail: avd.detail,
                pid: None,
            };
            if let Some(prev) = previous.iter().find(|item| item.name == device.name) {
                device.pid = prev.pid;
                match prev.phase {
                    Phase::Launching => {
                        device.phase = Phase::Launching;
                        device.summary = prev.summary.clone();
                        device.detail = prev.detail.clone();
                    }
                    Phase::Starting => keep_starting(&mut device, prev, &mut self.error, &mut self.dismissed),
                    Phase::Stopping => {
                        if active(&device.presence) {
                            device.phase = Phase::Stopping;
                            device.summary = prev.summary.clone();
                        } else {
                            device.pid = None;
                            device.summary = None;
                            device.detail = None;
                        }
                    }
                    Phase::Failed => {
                        if manual && matches!(device.presence, Presence::Off) {
                            device.phase = Phase::Idle;
                        } else if !active(&device.presence) {
                            device.phase = Phase::Failed;
                            device.summary = prev.summary.clone();
                            device.detail = prev.detail.clone();
                        }
                    }
                    Phase::Idle => {}
                }
            }
            devices.push(device);
        }

        self.devices = devices;
        if self
            .selected
            .as_ref()
            .is_some_and(|name| !self.devices.iter().any(|device| &device.name == name))
        {
            self.selected = None;
        }

        if manual {
            if let Some(detail) = self.devices.iter().find_map(|device| device.detail.clone()) {
                self.report(detail);
            }
        }
    }

    fn report(&mut self, message: String) {
        if self.dismissed.as_ref() == Some(&message) {
            return;
        }
        self.dismissed = None;
        self.error = Some(message);
    }
}

fn keep_starting(
    device: &mut Device,
    prev: &Device,
    error: &mut Option<String>,
    dismissed: &mut Option<String>,
) {
    if active(&device.presence) {
        device.phase = Phase::Idle;
        device.pid = None;
        return;
    }
    let Some(pid) = prev.pid else {
        device.phase = Phase::Launching;
        device.summary = prev.summary.clone();
        return;
    };
    if let Some(why) = emulator::startup_failure_if_exited(&device.name, pid) {
        device.phase = Phase::Failed;
        device.pid = None;
        device.presence = Presence::Off;
        device.summary = Some("The emulator exited before Android came online.".into());
        device.detail = Some(why.clone());
        if dismissed.as_ref() != Some(&why) {
            *dismissed = None;
            *error = Some(why);
        }
    } else {
        device.phase = Phase::Starting;
        device.pid = Some(pid);
        device.summary = Some(format!(
            "The emulator process (pid {pid}) is up, but adb does not see a device yet. Android can take a minute to boot. This is not a failure unless the process exits."
        ));
        device.detail = None;
    }
}

fn device_row(
    device: &Device,
    selected_name: Option<&str>,
    radius: [f32; 4],
    spacing: u16,
) -> Element<'static, Message> {
    let selected = selected_name == Some(device.name.as_str());
    let mut lines = widget::Column::with_capacity(3)
        .push(widget::text::body(device.name.clone()))
        .push(widget::text::caption(status_label(device)))
        .width(Length::Fill)
        .spacing(2);
    if let Some(summary) = &device.summary {
        lines = lines.push(widget::text::caption(summary.clone()));
    }

    let info = widget::row![widget::icon(icons::android()).size(22), lines]
        .align_y(Alignment::Center)
        .spacing(spacing)
        .width(Length::Fill);

    let select = widget::button::custom(info)
        .width(Length::Fill)
        .on_press(Message::Select(device.name.clone()))
        .selected(selected)
        .class(if selected {
            cosmic::theme::Button::ListItem(radius)
        } else {
            cosmic::theme::Button::Text
        });

    let mut row = widget::row![select]
        .align_y(Alignment::Center)
        .spacing(spacing)
        .width(Length::Fill);
    if let Some(action) = action_button(device) {
        row = row.push(action);
    }
    row.into()
}

fn action_button(device: &Device) -> Option<Element<'static, Message>> {
    if matches!(device.phase, Phase::Stopping | Phase::Launching) {
        return None;
    }
    if can_start(device) {
        return Some(
            widget::button::icon(icons::play())
                .tooltip("Start")
                .extra_small()
                .on_press(Message::Start(device.name.clone()))
                .into(),
        );
    }
    if stop_target(device).is_some() {
        return Some(
            widget::button::icon(icons::stop())
                .tooltip("Stop")
                .extra_small()
                .on_press(Message::Stop(device.name.clone()))
                .into(),
        );
    }
    None
}

fn can_start(device: &Device) -> bool {
    !matches!(
        device.phase,
        Phase::Launching | Phase::Starting | Phase::Stopping
    ) && matches!(device.presence, Presence::Off | Presence::Unknown)
}

fn active(presence: &Presence) -> bool {
    matches!(
        presence,
        Presence::Running { .. } | Presence::Offline { .. } | Presence::Unauthorized { .. }
    )
}

fn stop_target(device: &Device) -> Option<StopTarget> {
    match &device.presence {
        Presence::Running { serial }
        | Presence::Offline { serial }
        | Presence::Unauthorized { serial } => Some(StopTarget::Adb {
            serial: serial.clone(),
        }),
        Presence::Off | Presence::Unknown => device
            .pid
            .filter(|pid| emulator::process_alive(*pid))
            .map(|pid| StopTarget::Pid { pid }),
    }
}

fn already_running_reason(device: &Device) -> String {
    match (&device.phase, &device.presence) {
        (_, Presence::Running { serial }) => format!(
            "{} is already running as {serial}, so it was not started again. Press Stop to shut it down first.",
            device.name
        ),
        (_, Presence::Offline { serial }) => format!(
            "{} ({serial}) is already open, but the device is offline. Starting another copy would fight the existing process. Press Stop, wait until it leaves the list, then start it again.",
            device.name
        ),
        (_, Presence::Unauthorized { serial }) => format!(
            "{} ({serial}) is already open, but adb is not authorized to control it. Accept the debugging prompt on the emulator instead of starting a second copy.",
            device.name
        ),
        (Phase::Launching | Phase::Starting, _) => format!(
            "{} is already starting. Wait until it says Running, or press Stop if the process is up and you want to cancel it.",
            device.name
        ),
        (Phase::Stopping, _) => format!(
            "{} is shutting down. Wait until it says Off before starting it again.",
            device.name
        ),
        _ => format!(
            "{} cannot be started right now. Press Read emulators to refresh its state.",
            device.name
        ),
    }
}

fn status_label(device: &Device) -> String {
    match device.phase {
        Phase::Launching => "Starting".into(),
        Phase::Starting => match device.pid {
            Some(pid) => format!("Starting · pid {pid}"),
            None => "Starting".into(),
        },
        Phase::Stopping => "Stopping".into(),
        Phase::Failed => "Failed".into(),
        Phase::Idle => match &device.presence {
            Presence::Off => "Off".into(),
            Presence::Unknown => "Unknown".into(),
            Presence::Running { serial } => format!("Running · {serial}"),
            Presence::Offline { serial } => format!("Offline · {serial}"),
            Presence::Unauthorized { serial } => format!("Unauthorized · {serial}"),
        },
    }
}

fn scan_task(manual: bool) -> Task<Message> {
    cosmic::task::future(async move {
        let scan = emulator::scan();
        Message::Scanned { manual, scan }
    })
}

fn main() -> cosmic::iced::Result {
    cosmic::applet::run::<EmulatorApplet>(())
}
