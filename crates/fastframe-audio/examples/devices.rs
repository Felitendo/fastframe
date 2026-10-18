//! Lists the output and input devices a picker would offer, with their ids:
//! `cargo run -p fastframe-audio --example devices`. Nothing is opened.

use fastframe_audio::DeviceInfo;

#[allow(clippy::print_stdout)]
fn main() -> Result<(), fastframe_audio::OpenError> {
    let print = |what: &str, devices: &[DeviceInfo]| {
        println!("{what}:");
        for device in devices {
            let default = if device.is_default { " (default)" } else { "" };
            println!("  {}{default}\n    id: {}", device.name, device.id);
        }
    };
    let outputs = fastframe_audio::output_devices()?;
    let inputs = fastframe_audio::input_devices()?;
    println!("through {}", fastframe_audio::host());
    print("outputs", &outputs);
    print("inputs", &inputs);
    Ok(())
}
