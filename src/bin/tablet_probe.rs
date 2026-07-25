#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let devices = sketchpad::x11_tablet::discover()?;
    if devices.is_empty() {
        println!("no pressure-capable XInput2 pen or eraser devices found");
        return Ok(());
    }

    for device in devices {
        println!(
            "device={} tool={:?} name={:?}",
            device.id, device.tool, device.name
        );
        for axis in device.axes {
            println!(
                "  axis={} label={:?} min={:.3} max={:.3} resolution={}",
                axis.number, axis.label, axis.min, axis.max, axis.resolution
            );
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("tablet_probe currently supports Linux/X11 only");
}
