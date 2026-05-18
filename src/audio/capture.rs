use anyhow::{anyhow, Result};
use bytes::Bytes;
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use wasapi::{
    initialize_mta, Device, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat,
};

use crate::state::CaptureCtl;

pub const SAMPLE_RATE: usize = 48_000;
pub const CHANNELS: usize = 2;

/// Enumerate Active render devices (friendly names) -- used to build the tray submenu.
/// Must be called on a COM-MTA-initialized thread.
pub fn enumerate_render_devices() -> Result<Vec<String>> {
    let enumerator = DeviceEnumerator::new().map_err(|e| anyhow!("enumerator: {e}"))?;
    let mut names = Vec::new();
    for dev_res in &enumerator
        .get_device_collection(&Direction::Render)
        .map_err(|e| anyhow!("get_device_collection: {e}"))?
    {
        let dev = dev_res.map_err(|e| anyhow!("device iter: {e}"))?;
        if !matches!(dev.get_state(), Ok(wasapi::DeviceState::Active)) {
            continue;
        }
        if let Ok(name) = dev.get_friendlyname() {
            names.push(name);
        }
    }
    Ok(names)
}

fn pick_device(enumerator: &DeviceEnumerator, want: Option<&str>) -> Result<Device> {
    eprintln!("[capture] available render devices:");
    let mut matched: Option<Device> = None;
    for dev_res in &enumerator
        .get_device_collection(&Direction::Render)
        .map_err(|e| anyhow!("get_device_collection: {e}"))?
    {
        let dev = dev_res.map_err(|e| anyhow!("device iter: {e}"))?;
        let name = dev.get_friendlyname().unwrap_or_else(|_| "<unknown>".into());
        let state = format!("{:?}", dev.get_state().unwrap_or(wasapi::DeviceState::NotPresent));
        let is_match = want
            .map(|w| name.to_lowercase().contains(&w.to_lowercase()))
            .unwrap_or(false);
        let marker = if is_match { "  >> " } else { "     " };
        eprintln!("{marker}[{state}] {name}");
        if is_match && matched.is_none() {
            matched = Some(dev);
        }
    }

    if let Some(dev) = matched {
        return Ok(dev);
    }
    if let Some(want) = want {
        eprintln!(
            "[capture] selection '{want}' did not match any device; falling back to default."
        );
    }
    enumerator
        .get_default_device(&Direction::Render)
        .map_err(|e| anyhow!("default render device: {e}"))
}

pub fn run_capture(tx: broadcast::Sender<Bytes>, ctl: Arc<CaptureCtl>) -> Result<()> {
    if initialize_mta().is_err() {
        return Err(anyhow!("CoInitializeEx(MTA) failed"));
    }
    let enumerator = DeviceEnumerator::new().map_err(|e| anyhow!("enumerator: {e}"))?;

    loop {
        let want = ctl.selected_device.lock().unwrap().clone();
        ctl.restart.store(false, Ordering::SeqCst);
        match run_session(&enumerator, &tx, &ctl, want.as_deref()) {
            Ok(()) => eprintln!("[capture] restart requested -- swapping device."),
            Err(e) => {
                eprintln!("[capture] session error: {e}; retrying in 1s");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn run_session(
    enumerator: &DeviceEnumerator,
    tx: &broadcast::Sender<Bytes>,
    ctl: &CaptureCtl,
    want: Option<&str>,
) -> Result<()> {
    let device = pick_device(enumerator, want)?;
    let name = device
        .get_friendlyname()
        .unwrap_or_else(|_| "<unknown>".into());
    eprintln!("[capture] >>> capturing from: {name}");

    let mut audio_client = device
        .get_iaudioclient()
        .map_err(|e| anyhow!("get_iaudioclient: {e}"))?;

    let desired_format = WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE, CHANNELS, None);
    let blockalign = desired_format.get_blockalign() as usize;

    let (_def_time, min_time) = audio_client
        .get_device_period()
        .map_err(|e| anyhow!("get_device_period: {e}"))?;

    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: min_time,
    };
    audio_client
        .initialize_client(&desired_format, &Direction::Capture, &mode)
        .map_err(|e| anyhow!("initialize_client: {e}"))?;

    let h_event = audio_client
        .set_get_eventhandle()
        .map_err(|e| anyhow!("set_get_eventhandle: {e}"))?;

    let capture_client = audio_client
        .get_audiocaptureclient()
        .map_err(|e| anyhow!("get_audiocaptureclient: {e}"))?;

    audio_client
        .start_stream()
        .map_err(|e| anyhow!("start_stream: {e}"))?;
    eprintln!("[capture] stream started ({SAMPLE_RATE} Hz, {CHANNELS} ch, f32, {blockalign} byte/frame)");

    let mut byte_queue: VecDeque<u8> = VecDeque::with_capacity(1 << 16);
    let mut rms_acc: f64 = 0.0;
    let mut rms_samples: usize = 0;
    let mut last_rms_log = std::time::Instant::now();

    loop {
        if ctl.restart.load(Ordering::SeqCst) {
            let _ = audio_client.stop_stream();
            return Ok(());
        }

        capture_client
            .read_from_device_to_deque(&mut byte_queue)
            .map_err(|e| anyhow!("read_from_device_to_deque: {e}"))?;

        let drainable = (byte_queue.len() / blockalign) * blockalign;
        if drainable > 0 {
            let chunk: Vec<u8> = byte_queue.drain(..drainable).collect();
            for sample_bytes in chunk.chunks_exact(4) {
                let s = f32::from_le_bytes([
                    sample_bytes[0],
                    sample_bytes[1],
                    sample_bytes[2],
                    sample_bytes[3],
                ]) as f64;
                rms_acc += s * s;
                rms_samples += 1;
            }
            let _ = tx.send(Bytes::from(chunk));
        }

        if last_rms_log.elapsed() >= Duration::from_secs(1) {
            let rms = if rms_samples > 0 {
                (rms_acc / rms_samples as f64).sqrt()
            } else {
                0.0
            };
            let db = if rms > 1e-9 { 20.0 * rms.log10() } else { -120.0 };
            eprintln!("[capture] last 1s avg level: {db:>6.1} dBFS  ({rms_samples} samples)");
            rms_acc = 0.0;
            rms_samples = 0;
            last_rms_log = std::time::Instant::now();
        }

        if h_event.wait_for_event(200).is_err() {
            continue;
        }
    }
}
