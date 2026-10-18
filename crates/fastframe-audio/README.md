# fastframe-audio

Audio output for egui apps: one device stream that costs nothing while
paused, follows the default output, and reopens after a failure. The app's
own renderer fills it: a rodio mixer, a kira renderer, or a decoder of its
own.

Spotifast found that rodio's output stream keeps the device asking for
sound until it is dropped, so a paused player still ran the audio callback
and mixed silence many times a second, about 1% of a core on a Mac
(crmne/spotifast#636). This crate opens the cpal stream itself and pauses
it, so a paused output gets no callbacks at all and resumes in
microseconds. It came out of Spotifast's output, with what Solco and ZapFast
need from theirs.

## Usage

```rust
use fastframe_audio::{Maintained, Output, OutputOptions, Render};

struct Player { /* the app's mixer, decoder or kira renderer */ }

impl Render for Player {
    fn configure(&mut self, sample_rate: u32, channels: u16) {
        // Called before the first render, and again after any reopen, which
        // can be at another rate or channel count.
    }
    fn render(&mut self, out: &mut [f32]) {
        out.fill(0.0); // interleaved; silence when there is nothing to play
    }
}

// Asking the device can take a moment: open it off the UI thread.
let mut output = Output::open(OutputOptions::default(), Player { /* .. */ })?;
let clock = output.clock(); // Send + Sync, for any thread

output.pause();  // no callbacks, no CPU, the device stays open
output.resume(); // plays again at once

// Now and then on the thread that owns the output:
if let Maintained::Reopened { sample_rate, .. } = output.maintain() {
    // Another device, perhaps another rate.
}
for error in output.take_errors() {
    if error.is_fatal() { /* fail what is playing */ }
}
```

A renderer whose engine is made from the output, as kira's `Renderer` is
made with its manager, can be opened empty and take the engine afterwards:
have `render` write silence until the engine arrives over a one-shot channel
it checks without blocking (`try_recv`), as Solco does.

The renderer runs on the audio thread and must not block. The app reaches it
through its own handles (rodio's mixer, kira's manager, a channel), never
through the output, so nothing the app does can make the callback wait. It
stays the same across pauses and reopens, so what is playing carries on from
the same sample.

## Devices

`output_devices()` and `input_devices()` list what a device picker can
offer, each a `DeviceInfo` with an `id`, a `name` and whether it
`is_default`. Nothing is opened. Save the id (`DeviceId::as_str`) and open
the device by it later with `Device::Id(DeviceId::new(saved))`: it stays
the same when the device is renamed or the list changes order, and tells
apart two devices with the same name. It is the system's own id for the
device, without the system's name:

| System | Id | Example |
| --- | --- | --- |
| PipeWire | the node's `node.name` | `alsa_output.pci-0000_00_1f.3.analog-stereo` |
| PulseAudio | the sink or source name (under `pipewire-pulse`, the same `node.name`) | `alsa_output.pci-0000_00_1f.3.analog-stereo` |
| ALSA | the PCM name | `hw:CARD=PCH,DEV=0` |
| WASAPI | the endpoint id | `{0.0.0.00000000}.{guid}` |
| CoreAudio | the device's UID | `BuiltInSpeakerDevice` |

A saved id that matches nothing (the device is unplugged, or the settings
came from another machine) opens the default device, with a warning in the
log. A device is matched by its id for `Device::Id` and by its name for
`Device::Named`, never one for the other. `device_id()` gives the id of
the device a stream opened on; after `maintain()` returns `Reopened`, it
gives the new device's.

The listings leave out what is not a device of its own: cpal's PipeWire
default devices (`Device::Default` stands for them) and other apps' playback
streams, the monitors of outputs among the inputs, and ALSA's `null`
device. On PipeWire the default is found through its PulseAudio server; a
system without one marks no device the default. `output_device_names()`
still returns every name cpal lists, as before.

## The feeding thread

The callback runs ahead of ordinary work: on CoreAudio's real-time I/O
thread on macOS, on the WASAPI thread in MMCSS on Windows (the output joins
it as an "Audio" task), and on Linux on cpal's own thread, which the output
asks RTKit to make real-time on its first callback (or, refused, to raise
its nice level), as PipeWire does for its own threads. It does so only on
devices that wait for the hardware, by cpal's own rule: cpal's PipeWire and
PulseAudio hosts and ALSA's `hw` devices, not ALSA's `null` device, which
never waits, nor its plugins. A callback thread that runs for most of 40 ms
anyway takes back its raised priority itself, before the kernel's limit on
real-time CPU time would end the process. Without RTKit (a minimal system,
a Flatpak sandbox) the thread keeps ordinary priority and the outcome is
logged once.

`Clock::underruns()` counts the times the device played silence because the
callback came too late, by the device's own timeline of when each block
plays.

The thread that decodes and queues what the renderer plays does not run
ahead by itself, so a
busy or swapping machine can keep it waiting longer than the queue lasts,
and the callback plays silence: crackles and gaps that the device never
reports (crmne/spotifast#785). Call `fastframe_audio::raise_feeder_priority()`
once on that thread (librespot's player thread, a kira decoder, a decoding
worker of the app's own):

- macOS: the user-interactive QoS class, ahead of default, utility and
  background work;
- Windows: one step above normal priority;
- Linux: nice -5, which an unprivileged process is usually refused.

It is the policy Solco gives its audition decoder. It is best effort: it
returns whether the system granted it, logs a refusal once at debug level,
and the thread plays on as it was. Only that thread changes, so never call
it from a thread pool shared with other work.

## Options

`OutputOptions` (all have defaults):

- `device`: `Device::Default`, `Device::Id(id)` from `output_devices()`, or
  `Device::Named(name)` from `output_device_names()`. Only that device is
  ever opened; one that is missing falls back to the default, with a
  warning in the log.
- `channels`: `2` by default; `0` takes the device's own.
- `sample_rate`: a rate to try first, such as Spotify's 44.1 kHz; by default
  the device's own rate.
- `buffer`: the driver's own size, `Buffer::Fixed(size)` everywhere (PulseAudio
  otherwise targets about two seconds), or `Buffer::FixedOnWindows(size)`,
  for Windows' shared-mode underruns. A size is `BufferSize::Frames(n)` or a
  `BufferSize::Duration`. It is asked for as given, and clamped to the range
  the device reports only if the device refuses it: WASAPI in shared mode
  reports its 10 ms period as the range but takes any size.
- `max_block_frames`: render in blocks of at most this many frames, whatever
  the device's buffer, so a renderer's own clock moves in small steps.
- `follow_default`: move to a new default output (on macOS and Windows,
  checked every two seconds; PipeWire and PulseAudio move the stream
  themselves). On by default. On Windows the default output opens as the
  endpoint it is at that moment, not as WASAPI's virtual default device:
  that one stops activating for the rest of the process once its change
  notifications have run (RustAudio/cpal#1402), which left Spotifast silent
  after a Bluetooth headset connected (crmne/spotifast#689).
- `release_after`: let the device go after a pause this long (five minutes by
  default); the next `resume()` opens it again, and `maintain()` then reports
  `Reopened`. `maintain()` is what lets it go, so an app whose audio thread
  sleeps while paused calls it every few seconds during the pause, or the
  device stays open (still with no callbacks).

The output opens at the preferred rate, then at the device's own, each with
the fixed buffer when one was asked for, then with the driver's buffer, then
in any configuration the device lists. Every cpal sample format is filled
from the renderer's `f32`.

## Keeping it working

`maintain()` is cheap when nothing changed. It reopens a stream that failed
or whose device went away, or that stopped asking for sound without saying
so (its clock stood still for two seconds while playing, as some drivers do
when a device is rerouted), moves to a new default output, and lets the
device go after a long pause. Nothing happens between calls, so keep calling
it, playing or paused, at least every second or so. It returns what it did:

- `Unchanged`;
- `Reopened { device, sample_rate, channels, reason }`, with `reason` one of
  `Failed`, `DefaultChanged`, `Stalled` or `Resumed` (a reopen `resume()`
  made after an idle release, reported by the next `maintain()`);
- `Released`;
- `Failed(error)`. A failed reopen is not remembered: the next `maintain()`
  or `resume()` tries again.

`failed()` is a cheap check for a write path. `pause()` and `resume()` never
fail; a device that will not start shows up through `maintain()`.

## Errors

`take_errors()` drains what the device reported, each an `OutputError` whose
`kind()` is `Fatal` (the audio system is missing, access was denied, a
resource ran out, the configuration or operation is not supported) or
`Recoverable` (a glitch, a busy or vanished device, a rerouted stream,
refused real-time priority, an unclassified backend error). A fatal error is
reported even when the output has already reopened, so the app can fail what
was playing and let the next play use the reopened device.

## The clock

`Clock::played()` is how long the output has played: frames rendered at the
stream's rate, less the latency the device reports, so it counts what has
reached the device. It never goes backwards, stands still while paused, and
carries on across reopens (another device, another rate) without jumping.
Take differences between readings to follow a clip. `Clock::latency()` is the
latency itself, for lining up a picture with the sound.

## The audio system

`fastframe_audio::host()` names the audio system the process plays
through (`PipeWire`, `PulseAudio`, `ALSA`, `CoreAudio` or `WASAPI`), for the
log. The process chooses it the first time it opens or lists a device and
keeps that choice, so every stream and listing goes to the same system;
each still gets a fresh connection to it, so new devices show up and a
restarted sound server is reached again.

## Linux sound servers

On Linux the output goes to the sound server itself, never through ALSA's
`pipewire`, `pulse` or `default` plugins: cpal's PipeWire host, then its
PulseAudio host (pure Rust), then ALSA, whichever is running. Both server
hosts are always built in, so an app needs no feature for them, and only
they and ALSA's `hw` devices get the real-time callback.

Building on Linux needs ALSA's headers (`libasound2-dev`, `alsa-lib`), and
for the PipeWire host libpipewire's headers and clang (`libpipewire-0.3-dev
clang libclang-dev` on Debian and Ubuntu, `pipewire clang` on Arch).

The `pulseaudio` and `pipewire` features still exist, so apps that named
them keep building, but they no longer change anything.

## Example

`cargo run -p fastframe-audio --example idle` opens the default output with
a renderer that writes silence, and prints the render calls, the largest
callback and the clock while playing, paused and resumed, and how soon the
first callback follows `resume()`. `-- --buffer-ms 100` asks for a fixed
100 ms buffer instead of Solco's 512 frames. A paused output got no calls on
ALSA, PulseAudio, PipeWire and WASAPI alike.

## Not in this crate

Decoding, resampling, mixing, fades and visualiser taps belong to the
renderer, where rodio and kira already do them. Recording, and pausing other
media players while playing, stay in the app that needs them until a second
app does.
