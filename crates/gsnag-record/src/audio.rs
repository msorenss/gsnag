use super::Control;
use anyhow::{Context, Result};
use pipewire::{self as pw, properties::properties, spa};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const RATE: u64 = 48_000;
#[derive(Default)]
pub struct Samples {
    chunks: VecDeque<(u64, Vec<[f32; 2]>)>,
    pub received: bool,
    pub error: Option<String>,
    pub last_end: u64,
}
impl Samples {
    pub fn mix(&mut self, start: u64, into: &mut [[f32; 2]], gain: f32) {
        let end = start + into.len() as u64;
        while self
            .chunks
            .front()
            .is_some_and(|(at, samples)| *at + samples.len() as u64 <= start)
        {
            self.chunks.pop_front();
        }
        for (at, samples) in &self.chunks {
            let left = start.max(*at);
            let right = end.min(*at + samples.len() as u64);
            for n in left..right {
                for c in 0..2 {
                    into[(n - start) as usize][c] += samples[(n - *at) as usize][c] * gain;
                }
            }
        }
    }
}
pub struct AudioCapture {
    pub samples: Arc<Mutex<Samples>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl AudioCapture {
    pub fn start(system: bool, target: Option<String>, control: Arc<Control>) -> Self {
        let samples = Arc::new(Mutex::new(Samples::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let sink = samples.clone();
        let shutdown = stop.clone();
        let thread = std::thread::spawn(move || {
            if let Err(error) = capture(system, target, &control, &shutdown, sink.clone()) {
                sink.lock().unwrap().error = Some(format!("{error:#}"));
            }
        });
        Self {
            samples,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
fn capture(
    system: bool,
    target: Option<String>,
    control: &Arc<Control>,
    stop: &AtomicBool,
    sink: Arc<Mutex<Samples>>,
) -> Result<()> {
    pw::init();
    let loop_ = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&loop_, None)?;
    let core = context
        .connect_rc(None)
        .context("Cannot connect to PipeWire")?;
    let mut props = properties! { "media.type"=>"Audio", "media.category"=>"Capture", "media.role"=>"Production", "node.name"=>"gsnag-record", "node.description"=>"gsnag recording", "stream.capture.sink"=>if system {"true"} else {"false"} };
    if let Some(target) = target {
        props.insert("target.object", target);
    }
    let stream = pw::stream::StreamBox::new(&core, "gsnag", props)?;
    let clock = control.clone();
    let errors = sink.clone();
    let mut next_pts = None;
    let _listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(move |_, _, _, state| {
            if let pw::stream::StreamState::Error(e) = state {
                errors.lock().unwrap().error = Some(e.to_string());
            }
        })
        .process(move |stream, _| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            if clock.paused() {
                next_pts = None;
                return;
            }
            let datas = buffer.datas_mut();
            let Some(data) = datas.first_mut() else {
                return;
            };
            let offset = data.chunk().offset() as usize;
            let size = data.chunk().size() as usize;
            let Some(bytes) = data.data() else {
                return;
            };
            let Some(bytes) = bytes.get(offset..offset.saturating_add(size)) else {
                return;
            };
            if bytes.len() < 8 {
                return;
            }
            if !clock.started() {
                sink.lock().unwrap().received = true;
                next_pts = None;
                return;
            }
            let samples: Vec<[f32; 2]> = bytes
                .chunks_exact(8)
                .map(|b| {
                    let sample = |part: &[u8]| {
                        let v = f32::from_le_bytes(part.try_into().unwrap());
                        if v.is_finite() {
                            v.clamp(-1.0, 1.0)
                        } else {
                            0.0
                        }
                    };
                    [sample(&b[..4]), sample(&b[4..8])]
                })
                .collect();
            let end = (clock.elapsed().as_secs_f64() * RATE as f64) as u64;
            let observed = end.saturating_sub(samples.len() as u64);
            // Keep sample positions continuous despite callback scheduling jitter.
            // Re-anchor after a pause or a device discontinuity greater than 100 ms.
            let at = next_pts
                .filter(|next: &u64| next.abs_diff(observed) < RATE / 10)
                .unwrap_or(observed);
            next_pts = Some(at + samples.len() as u64);
            let mut sink = sink.lock().unwrap();
            sink.received = true;
            sink.last_end = end;
            sink.chunks.push_back((at, samples));
            while sink
                .chunks
                .front()
                .is_some_and(|(start, _)| start.saturating_add(RATE * 2) < end)
            {
                sink.chunks.pop_front();
            }
            // Bound memory even if a broken device timestamps many tiny buffers alike.
            while sink.chunks.len() > 512 {
                sink.chunks.pop_front();
            }
        })
        .register()?;
    let mut format = spa::param::audio::AudioInfoRaw::new();
    format.set_format(spa::param::audio::AudioFormat::F32LE);
    format.set_rate(RATE as u32);
    format.set_channels(2);
    let mut position = [0; 64];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    format.set_position(position);
    let object = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: format.into(),
    };
    let bytes = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map_err(|e| anyhow::anyhow!("Audio format: {e:?}"))?
    .0
    .into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&bytes).context("Invalid audio format")?];
    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    while !stop.load(Ordering::Relaxed) {
        loop_.loop_().iterate(Duration::from_millis(50));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mixes_offsets_and_trims_consumed_samples() {
        let mut source = Samples::default();
        source.chunks.push_back((2, vec![[0.5, -0.5]; 4]));
        let mut mixed = vec![[0.0; 2]; 4];
        source.mix(0, &mut mixed, 0.5);
        assert_eq!(
            mixed,
            vec![[0.0; 2], [0.0; 2], [0.25, -0.25], [0.25, -0.25]]
        );
        let mut tail = vec![[0.1; 2]; 4];
        source.mix(4, &mut tail, 0.5);
        assert_eq!(tail, vec![[0.35, -0.15], [0.35, -0.15], [0.1; 2], [0.1; 2]]);
        source.mix(6, &mut tail, 1.0);
        assert!(source.chunks.is_empty());
    }
}
