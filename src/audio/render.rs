use anyhow::{anyhow, Result};
use bytes::Bytes;
use std::collections::VecDeque;
use tokio::sync::{mpsc, oneshot};
use wasapi::{
    Device, Direction, DeviceEnumerator, SampleType, StreamMode, WaveFormat, initialize_mta,
};

use super::capture::{CHANNELS, SAMPLE_RATE};

const VBCABLE_HINT: &str = "CABLE Input";

/// Messages the WS layer hands to the render thread.
/// `Pcm` carries audio bytes; `Drain` requests an ack once all previously-sent
/// PCM has been pushed through WASAPI to VB-CABLE -- used by PTT-stop so the
/// tail of the user's speech is actually heard before Alt fires.
pub enum MicMsg {
    Pcm(Bytes),
    Drain(oneshot::Sender<()>),
}

/// Find the VB-CABLE virtual playback device by friendly name.
/// Returns None and logs a clear hint if not present.
fn pick_cable_input(enumerator: &DeviceEnumerator) -> Result<Option<Device>> {
    for dev_res in &enumerator
        .get_device_collection(&Direction::Render)
        .map_err(|e| anyhow!("get_device_collection: {e}"))?
    {
        let dev = dev_res.map_err(|e| anyhow!("device iter: {e}"))?;
        let name = dev
            .get_friendlyname()
            .unwrap_or_else(|_| "<unknown>".into());
        if name.contains(VBCABLE_HINT) {
            return Ok(Some(dev));
        }
    }
    Ok(None)
}

/// Blocking WASAPI render thread that takes PCM frames from the channel and
/// writes them to the VB-CABLE Input virtual device. Windows apps that pick
/// "CABLE Output" as their microphone will hear this audio.
///
/// Returns early (and warns) if VB-CABLE isn't installed -- the audio-out
/// direction still works, the mic direction just won't be usable until the
/// user installs the driver.
pub fn run_render(mut rx: mpsc::Receiver<MicMsg>) -> Result<()> {
    let hr = initialize_mta();
    if hr.is_err() {
        return Err(anyhow!("CoInitializeEx failed: {hr:?}"));
    }

    let enumerator = DeviceEnumerator::new().map_err(|e| anyhow!("enumerator: {e}"))?;
    let device = match pick_cable_input(&enumerator)? {
        Some(d) => d,
        None => {
            eprintln!("[render] VB-CABLE Input device not found -- mic-direction disabled.");
            eprintln!("[render] Install VB-CABLE: https://vb-audio.com/Cable/index.htm");
            // Drain the channel forever so senders don't block; mic data is just dropped.
            while let Some(msg) = rx.blocking_recv() {
                if let MicMsg::Drain(ack) = msg {
                    let _ = ack.send(());
                }
            }
            return Ok(());
        }
    };

    let name = device
        .get_friendlyname()
        .unwrap_or_else(|_| "<unknown>".into());
    eprintln!("[render] rendering iPhone mic into: {name}");

    let mut audio_client = device
        .get_iaudioclient()
        .map_err(|e| anyhow!("get_iaudioclient: {e}"))?;

    let desired_format = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        SAMPLE_RATE,
        CHANNELS,
        None,
    );
    let blockalign = desired_format.get_blockalign() as usize;

    let (def_time, _min_time) = audio_client
        .get_device_period()
        .map_err(|e| anyhow!("get_device_period: {e}"))?;

    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: def_time,
    };
    audio_client
        .initialize_client(&desired_format, &Direction::Render, &mode)
        .map_err(|e| anyhow!("initialize_client: {e}"))?;

    let h_event = audio_client
        .set_get_eventhandle()
        .map_err(|e| anyhow!("set_get_eventhandle: {e}"))?;

    let render_client = audio_client
        .get_audiorenderclient()
        .map_err(|e| anyhow!("get_audiorenderclient: {e}"))?;

    let buffer_total = audio_client
        .get_buffer_size()
        .map_err(|e| anyhow!("get_buffer_size: {e}"))? as usize;

    audio_client
        .start_stream()
        .map_err(|e| anyhow!("start_stream: {e}"))?;
    eprintln!("[render] stream started ({SAMPLE_RATE} Hz, {CHANNELS} ch, f32, buffer={buffer_total} frames)");

    let mut queue: VecDeque<u8> = VecDeque::with_capacity(1 << 16);
    let mut pending_drain: Option<oneshot::Sender<()>> = None;

    loop {
        // Pull as many pending messages as available without blocking.
        // Stop accumulating once we see a Drain so we can process it after
        // flushing the bytes that arrived before it.
        while pending_drain.is_none() {
            match rx.try_recv() {
                Ok(MicMsg::Pcm(b)) => queue.extend(b.iter().copied()),
                Ok(MicMsg::Drain(ack)) => pending_drain = Some(ack),
                Err(_) => break,
            }
        }

        // If we have nothing buffered AND no drain pending, block for first message.
        if queue.is_empty() && pending_drain.is_none() {
            match rx.blocking_recv() {
                Some(MicMsg::Pcm(b)) => queue.extend(b.iter().copied()),
                Some(MicMsg::Drain(ack)) => pending_drain = Some(ack),
                None => return Ok(()), // channel closed
            }
        }

        // Write a single chunk of available WASAPI space.
        if !queue.is_empty() {
            let available_frames = audio_client
                .get_available_space_in_frames()
                .map_err(|e| anyhow!("get_available_space_in_frames: {e}"))?
                as usize;
            if available_frames == 0 {
                let _ = h_event.wait_for_event(200);
                continue;
            }

            let bytes_needed = available_frames * blockalign;
            let mut data = vec![0u8; bytes_needed];
            let fill_bytes = queue.len().min(bytes_needed);
            for (slot, byte) in data.iter_mut().take(fill_bytes).zip(queue.drain(..fill_bytes)) {
                *slot = byte;
            }
            // Remaining slots stay zero (silence) -- safe to write.

            render_client
                .write_to_device(available_frames, &data, None)
                .map_err(|e| anyhow!("write_to_device: {e}"))?;

            if h_event.wait_for_event(200).is_err() {
                continue;
            }
            continue;
        }

        // queue is empty -- if there's a pending drain, wait for WASAPI to
        // actually play out (padding == 0) before acknowledging.
        if let Some(ack) = pending_drain.take() {
            loop {
                let avail = audio_client
                    .get_available_space_in_frames()
                    .map_err(|e| anyhow!("get_available_space_in_frames: {e}"))?
                    as usize;
                if avail >= buffer_total {
                    break;
                }
                if h_event.wait_for_event(200).is_err() {
                    // Timeout; loop and re-check rather than spinning.
                    continue;
                }
            }
            eprintln!("[render] drain complete, acking");
            let _ = ack.send(());
        }
    }
}
