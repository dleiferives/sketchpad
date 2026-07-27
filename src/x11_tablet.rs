use crate::input::{
    TabletAxisInfo, TabletDeviceInfo, TabletEvent, TabletPhase, TabletSample, ToolKind,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    collections::HashMap,
    error::Error,
    fmt,
    thread::{self, JoinHandle},
    time::Instant,
};
use winit::{event_loop::EventLoopProxy, window::Window};
use x11rb::{
    connection::Connection,
    protocol::{
        xinput::{self, DeviceClassData, EventMask, XIEventMask},
        xproto, Event,
    },
};

const PRESSURE_LABEL: &str = "Abs Pressure";
const TILT_X_LABEL: &str = "Abs Tilt X";
const TILT_Y_LABEL: &str = "Abs Tilt Y";
const DISTANCE_LABEL: &str = "Abs Distance";

#[derive(Debug)]
pub struct TabletBackend {
    devices: Vec<TabletDeviceInfo>,
    _thread: JoinHandle<()>,
}

impl TabletBackend {
    pub fn devices(&self) -> &[TabletDeviceInfo] {
        &self.devices
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabletError(String);

impl TabletError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for TabletError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Error for TabletError {}

pub fn discover() -> Result<Vec<TabletDeviceInfo>, TabletError> {
    let (connection, _) =
        x11rb::connect(None).map_err(|error| TabletError::new(error.to_string()))?;
    discover_on_connection(&connection)
}

pub fn start(
    window: &Window,
    proxy: EventLoopProxy<TabletEvent>,
) -> Result<TabletBackend, TabletError> {
    let window_id = x11_window_id(window)?;
    let (connection, _) =
        x11rb::connect(None).map_err(|error| TabletError::new(error.to_string()))?;
    let devices = discover_on_connection(&connection)?;
    if devices.is_empty() {
        return Err(TabletError::new(
            "XInput2 reported no enabled pen or eraser with a pressure axis",
        ));
    }

    xinput::xi_query_version(&connection, 2, 0)
        .map_err(|error| TabletError::new(error.to_string()))?
        .reply()
        .map_err(|error| TabletError::new(error.to_string()))?;

    let device_id: u16 = xinput::Device::ALL.into();
    let mask = EventMask {
        deviceid: device_id,
        mask: vec![XIEventMask::MOTION | XIEventMask::BUTTON_PRESS | XIEventMask::BUTTON_RELEASE],
    };
    xinput::xi_select_events(&connection, window_id, &[mask])
        .map_err(|error| TabletError::new(error.to_string()))?
        .check()
        .map_err(|error| TabletError::new(error.to_string()))?;
    connection
        .flush()
        .map_err(|error| TabletError::new(error.to_string()))?;

    let thread_devices = devices.clone();
    let backend_thread = thread::Builder::new()
        .name("sketchpad-xinput2".to_owned())
        .spawn(move || event_thread(connection, thread_devices, proxy))
        .map_err(|error| TabletError::new(error.to_string()))?;

    Ok(TabletBackend {
        devices,
        _thread: backend_thread,
    })
}

fn discover_on_connection<C: Connection>(
    connection: &C,
) -> Result<Vec<TabletDeviceInfo>, TabletError> {
    xinput::xi_query_version(connection, 2, 0)
        .map_err(|error| TabletError::new(error.to_string()))?
        .reply()
        .map_err(|error| TabletError::new(error.to_string()))?;
    let reply = xinput::xi_query_device(connection, xinput::Device::ALL)
        .map_err(|error| TabletError::new(error.to_string()))?
        .reply()
        .map_err(|error| TabletError::new(error.to_string()))?;
    let mut atom_names: HashMap<u32, String> = HashMap::new();
    let mut devices = Vec::new();

    for device in reply.infos {
        if !device.enabled {
            continue;
        }
        let name = String::from_utf8_lossy(&device.name).into_owned();
        let lowercase_name = name.to_ascii_lowercase();
        let tool = classify_tool(&lowercase_name);
        let Some(tool) = tool else {
            continue;
        };

        let mut axes = Vec::new();
        for class in device.classes {
            let DeviceClassData::Valuator(valuator) = class.data else {
                continue;
            };
            let label = if valuator.label == x11rb::NONE {
                "Unlabeled".to_owned()
            } else if let Some(label) = atom_names.get(&valuator.label) {
                label.clone()
            } else {
                let label = match xproto::get_atom_name(connection, valuator.label)
                    .map_err(|error| TabletError::new(error.to_string()))?
                    .reply()
                {
                    Ok(reply) => String::from_utf8_lossy(&reply.name).into_owned(),
                    Err(_) => format!("Atom {}", valuator.label),
                };
                atom_names.insert(valuator.label, label.clone());
                label
            };
            axes.push(TabletAxisInfo {
                number: valuator.number,
                label,
                min: fp3232(valuator.min),
                max: fp3232(valuator.max),
                resolution: valuator.resolution,
            });
        }

        if axes
            .iter()
            .any(|axis| axis.label.eq_ignore_ascii_case(PRESSURE_LABEL))
        {
            devices.push(TabletDeviceInfo {
                id: device.deviceid,
                name,
                tool,
                axes,
            });
        }
    }

    Ok(devices)
}

fn event_thread<C: Connection>(
    connection: C,
    devices: Vec<TabletDeviceInfo>,
    proxy: EventLoopProxy<TabletEvent>,
) {
    let mut states: HashMap<u16, DeviceState> = devices
        .into_iter()
        .map(|device| (device.id, DeviceState::new(device)))
        .collect();
    let mut clock = TimestampUnwrapper::default();

    loop {
        let event = match connection.wait_for_event() {
            Ok(event) => event,
            Err(error) => {
                let _ = proxy.send_event(TabletEvent::BackendError(error.to_string()));
                break;
            }
        };
        let backend_received_at = Instant::now();
        let event = match event {
            Event::XinputMotion(event) => tablet_event(
                &mut states,
                &mut clock,
                &event,
                EventKind::Motion,
                backend_received_at,
            ),
            Event::XinputButtonPress(event) if event.detail == 1 => tablet_event(
                &mut states,
                &mut clock,
                &event,
                EventKind::ButtonPress,
                backend_received_at,
            ),
            Event::XinputButtonRelease(event) if event.detail == 1 => tablet_event(
                &mut states,
                &mut clock,
                &event,
                EventKind::ButtonRelease,
                backend_received_at,
            ),
            _ => None,
        };

        if let Some(event) = event {
            if proxy.send_event(event).is_err() {
                break;
            }
        }
    }
}

#[derive(Clone, Copy)]
enum EventKind {
    Motion,
    ButtonPress,
    ButtonRelease,
}

fn tablet_event(
    states: &mut HashMap<u16, DeviceState>,
    clock: &mut TimestampUnwrapper,
    event: &xinput::ButtonPressEvent,
    kind: EventKind,
    backend_received_at: Instant,
) -> Option<TabletEvent> {
    let state = states.get_mut(&event.sourceid)?;
    let button_state = match kind {
        EventKind::ButtonPress => Some(true),
        EventKind::ButtonRelease => Some(false),
        EventKind::Motion => None,
    };
    if button_state.is_some_and(|down| !state.accept_button_event(down, event.time)) {
        return None;
    }
    state.update_axes(&event.valuator_mask, &event.axisvalues);
    let timestamp_millis = clock.unwrap(event.time);

    let phase = match kind {
        EventKind::Motion if state.down => TabletPhase::Move,
        EventKind::Motion => TabletPhase::Hover,
        EventKind::ButtonPress => {
            state.down = true;
            TabletPhase::Down
        }
        EventKind::ButtonRelease => TabletPhase::Up,
    };
    let sample = state.sample(
        [
            event.event_x as f32 / 65_536.0,
            event.event_y as f32 / 65_536.0,
        ],
        timestamp_millis,
    );
    if matches!(kind, EventKind::ButtonRelease) {
        state.down = false;
    }

    Some(TabletEvent::Sample {
        phase,
        sample,
        backend_received_at,
    })
}

struct DeviceState {
    info: TabletDeviceInfo,
    values: HashMap<u16, f64>,
    down: bool,
    recent_button_events: [Option<(bool, u32)>; 8],
    next_button_event: usize,
}

impl DeviceState {
    fn new(info: TabletDeviceInfo) -> Self {
        Self {
            info,
            values: HashMap::new(),
            down: false,
            recent_button_events: [None; 8],
            next_button_event: 0,
        }
    }

    fn accept_button_event(&mut self, down: bool, timestamp: u32) -> bool {
        let event = (down, timestamp);
        if self.recent_button_events.contains(&Some(event)) {
            return false;
        }
        self.recent_button_events[self.next_button_event] = Some(event);
        self.next_button_event = (self.next_button_event + 1) % self.recent_button_events.len();
        true
    }

    fn update_axes(&mut self, masks: &[u32], values: &[xinput::Fp3232]) {
        let mut values = values.iter();
        for (word_index, mask) in masks.iter().copied().enumerate() {
            for bit in 0..u32::BITS {
                if mask & (1 << bit) == 0 {
                    continue;
                }
                let Some(value) = values.next() else {
                    return;
                };
                let axis = word_index as u16 * u32::BITS as u16 + bit as u16;
                self.values.insert(axis, fp3232(*value));
            }
        }
    }

    fn sample(&self, position: [f32; 2], timestamp_millis: u64) -> TabletSample {
        TabletSample {
            device_id: self.info.id,
            tool: self.info.tool,
            position,
            pressure: self.normalized_axis(PRESSURE_LABEL, false),
            tilt: [
                self.normalized_axis(TILT_X_LABEL, true),
                self.normalized_axis(TILT_Y_LABEL, true),
            ],
            distance: self.normalized_axis(DISTANCE_LABEL, false),
            timestamp_millis,
        }
    }

    fn normalized_axis(&self, label: &str, signed: bool) -> f32 {
        let Some(axis) = self.info.axis(label) else {
            return 0.0;
        };
        let Some(value) = self.values.get(&axis.number) else {
            return 0.0;
        };
        if signed {
            axis.normalize_signed(*value)
        } else {
            axis.normalize_unit(*value)
        }
    }
}

#[derive(Default)]
struct TimestampUnwrapper {
    epoch: u64,
    last: Option<u32>,
}

impl TimestampUnwrapper {
    fn unwrap(&mut self, timestamp: u32) -> u64 {
        if let Some(last) = self.last {
            if timestamp < last && last - timestamp > i32::MAX as u32 {
                self.epoch += 1_u64 << 32;
            }
        }
        self.last = Some(timestamp);
        self.epoch + u64::from(timestamp)
    }
}

fn fp3232(value: xinput::Fp3232) -> f64 {
    f64::from(value.integral) + f64::from(value.frac) / 4_294_967_296.0
}

fn classify_tool(lowercase_name: &str) -> Option<ToolKind> {
    if lowercase_name.contains("cursor") {
        None
    } else if lowercase_name.contains("eraser") {
        Some(ToolKind::Eraser)
    } else if lowercase_name.contains("stylus") || lowercase_name.contains("pen") {
        Some(ToolKind::Pen)
    } else {
        None
    }
}

fn x11_window_id(window: &Window) -> Result<u32, TabletError> {
    let handle = window
        .window_handle()
        .map_err(|error| TabletError::new(error.to_string()))?;
    match handle.as_raw() {
        RawWindowHandle::Xlib(handle) => u32::try_from(handle.window)
            .map_err(|_| TabletError::new("Xlib window ID does not fit in 32 bits")),
        RawWindowHandle::Xcb(handle) => Ok(handle.window.get()),
        _ => Err(TabletError::new(
            "native tablet input currently requires an X11 window",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_point_conversion_handles_positive_and_negative_values() {
        assert_eq!(
            fp3232(xinput::Fp3232 {
                integral: 3,
                frac: 0x8000_0000,
            }),
            3.5
        );
        assert_eq!(
            fp3232(xinput::Fp3232 {
                integral: -4,
                frac: 0x8000_0000,
            }),
            -3.5
        );
    }

    #[test]
    fn timestamps_are_unwrapped_across_x_server_wraparound() {
        let mut clock = TimestampUnwrapper::default();
        assert_eq!(clock.unwrap(u32::MAX - 2), u64::from(u32::MAX - 2));
        assert_eq!(clock.unwrap(4), (1_u64 << 32) + 4);
    }

    #[test]
    fn tool_names_keep_pen_and_eraser_but_reject_puck_cursor() {
        assert_eq!(
            classify_tool("wacom intuos pro s pen stylus"),
            Some(ToolKind::Pen)
        );
        assert_eq!(
            classify_tool("wacom intuos pro s pen eraser"),
            Some(ToolKind::Eraser)
        );
        assert_eq!(classify_tool("wacom intuos pro s pen cursor"), None);
    }

    #[test]
    fn sparse_axis_packets_retain_pressure_and_normalize_tilt() {
        let info = TabletDeviceInfo {
            id: 18,
            name: "test pen".to_owned(),
            tool: ToolKind::Pen,
            axes: vec![
                TabletAxisInfo {
                    number: 2,
                    label: PRESSURE_LABEL.to_owned(),
                    min: 0.0,
                    max: 65_536.0,
                    resolution: 1,
                },
                TabletAxisInfo {
                    number: 3,
                    label: TILT_X_LABEL.to_owned(),
                    min: -64.0,
                    max: 63.0,
                    resolution: 57,
                },
            ],
        };
        let mut state = DeviceState::new(info);
        state.update_axes(
            &[1 << 2],
            &[xinput::Fp3232 {
                integral: 32_768,
                frac: 0,
            }],
        );
        state.update_axes(
            &[1 << 3],
            &[xinput::Fp3232 {
                integral: -32,
                frac: 0,
            }],
        );

        let sample = state.sample([10.0, 20.0], 30);
        assert_eq!(sample.pressure, 0.5);
        assert_eq!(sample.tilt[0], -0.5);
        assert_eq!(sample.position, [10.0, 20.0]);
        assert_eq!(sample.timestamp_millis, 30);
    }

    #[test]
    fn duplicate_tip_packets_at_the_same_timestamp_are_ignored() {
        let info = TabletDeviceInfo {
            id: 18,
            name: "test pen".to_owned(),
            tool: ToolKind::Pen,
            axes: Vec::new(),
        };
        let mut state = DeviceState::new(info);

        assert!(state.accept_button_event(true, 12_345));
        assert!(!state.accept_button_event(true, 12_345));
        assert!(state.accept_button_event(false, 12_678));
        assert!(!state.accept_button_event(false, 12_678));
        assert!(!state.accept_button_event(true, 12_345));
        assert!(state.accept_button_event(true, 12_900));
    }
}
