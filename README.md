# High-level Rust audio library for playback, capture, procedural audio, and DSP

Auditorium is centered around Hosts which manage audio devices and their processing. A host can create both playback and capture devices, and each device can be connected to an audio node graph.

Audio can be loaded from files or generated procedurally. Sources such as audio files, waves, noises, and pulses can be connected to DSP processing chains before being sent to a device.

A Host can manage any number of playback and capture devices, allowing multiple independent audio pipelines to run at the same time.

Playback devices also provide a way to track when audio has finished producing frames, making it possible to synchronize application logic with the actual end of playback.

Auditorium aims to provide a simple API while offering conveniences around device management, audio sources, DSP, and playback state.

Both capture and playback devices support device enumeration and selection. Most types exposed by a Host are Send + Sync and Clone.

## Supported targets

Out of the box, audiotorium supports the following targets:
- aarch64-apple-darwin
- aarch64-pc-windows-msvc
- aarch64-unknown-linux-gnu
- x86_64-pc-windows-gnu
- x86_64-pc-windows-msvc
- x86_64-unknown-linux-gnu

## Supported audio backends

For the selected targets, maudio supports the follwing OS backends:
- Wasapi
- DirectSound
- WinMM
- PulseAudio
- Alsa
- Jack
- CoreAudio

## Supported audio formats

- AAC
- ALAC
- FLAC
- MP3
- Opus
- Vorbis
- WAV / PCM
- ADPCM
- WavPack

# Examples

### Playback

This example plays a simple audio source from a file and applies some dsp to it.

```rust
    let host = Host::spawn()?;
    let device = host.build_playback_device()?.build()?;
    let audio1 = device.new_audio(&path)?;

    // We must keep the chain alive too
    let _chain = audio1.dsp().hpf(3000.0, 1).lpf(8000.0, 2).connect()?;

    device.start_device()?;
    audio1.start_audio()?;

    // This will only keeps track of current sound(s) and will shutdown when it ends
    // If you have a playlis, you must keep track of it separately
    // But this can be used this as a signal to start the next sound
    while device.is_producing() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    host.shutdown()?;
```

### Capture

When we start a capture device, the device itself is an audio source,
so we can apply dsp directly to the device.

We are mixing the audio recorded with another audio source, for 
demonstration purposes only.

At the moment, a capture device can only record to a `wav` file.

```rust
    let host = Host::spawn()?;
    let capt = host.build_capture_device()?.build("recording.wav")?;

    let audio = capt.new_audio(&path)?;

    capt.dsp().hpf(200.0, 1).connect()?;

    capt.start_device()?;
    audio.start_audio()?;

    std::thread::sleep(std::time::Duration::from_secs(3));
```

### Enumerate device

```rust
    let ctx = ContextBuilder::new().build()?;
    ctx.enumerate_devices(|dev, info| {
        if is_play && dev == DeviceType::Playback {
            eprintln!("Playback device: {idx}. {}", info.name());
            idx += 1;
        }
        if !is_play && dev == DeviceType::Capture {
            eprintln!("Capture device: {idx}. {}", info.name());
            idx += 1;
        }
        EnumerateControl::Continue
    })?;
```

### Capture on a specific device

A simple cli example would print out the available devices to the user.
Then, we would enumarate the devices again and stop at the device the user selected.
However, there is no guarantee that the order is stable over long periods of time,
especially with devices connecting / disconneting.

```rust
    // Assuming we have already selected a device
    let ctx = ContextBuilder::new().build()?;
    let mut selected = None;

    ctx.enumerate_devices(|device_type, info| {
        if device_type != DeviceType::Capture {
            return EnumerateControl::Continue;
        }

        if index == device_position {
            selected = Some(SelectedDevice {
                id: info.id().clone(),
                name: info.name().to_owned(),
            });

            return EnumerateControl::Stop;
        }

        index += 1;
        EnumerateControl::Continue
    })?;

    let host = Host::spawn()?;
    let device = host
        .build_capture_device()?
        .device_id(&selected.id)?
        .build(path)?;

    device.start_device()?;

    // We may want a loop that keeps the thread alive while recording
    while running.load(Ordering::Relaxed) {} 

    host.shutdown()?;
```