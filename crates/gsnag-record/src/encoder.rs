use anyhow::{Context, Result, bail};
use ff::{
    ChannelLayout, Packet, codec, encoder, format, frame,
    software::scaling,
    util::format::{
        pixel::Pixel,
        sample::{Sample, Type},
    },
};
use ffmpeg_next as ff;
use image::RgbaImage;
use std::path::Path;

pub struct Encoder {
    output: format::context::Output,
    video: encoder::Video,
    audio: Option<encoder::Audio>,
    scaler: scaling::Context,
    rgba: frame::Video,
    yuv: frame::Video,
    pub audio_frame_size: usize,
    fps: u32,
}
impl Encoder {
    pub fn new(
        path: &Path,
        container: &str,
        width: u32,
        height: u32,
        fps: u32,
        sound: bool,
    ) -> Result<Self> {
        ff::init()?;
        ff::log::set_level(ff::log::Level::Error);
        let mut output = format::output_as(path, container)?;
        let global = output
            .format()
            .flags()
            .contains(format::Flags::GLOBAL_HEADER);
        let codec = encoder::find_by_name("libx264").context("FFmpeg has no libx264 encoder")?;
        let mut video = codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()?;
        video.set_width(width);
        video.set_height(height);
        video.set_format(Pixel::YUV420P);
        video.set_time_base((1, fps as i32));
        video.set_frame_rate(Some((fps as i32, 1)));
        video.set_gop(fps * 2);
        video.set_max_b_frames(0);
        if global {
            video.set_flags(codec::Flags::GLOBAL_HEADER);
        }
        let mut options = ff::Dictionary::new();
        options.set("preset", "ultrafast");
        options.set("tune", "zerolatency");
        options.set("crf", "23");
        options.set("threads", "2");
        let video = video.open_with(options)?;
        {
            let mut stream = output.add_stream(codec)?;
            stream.set_parameters(&video);
            stream.set_time_base((1, fps as i32));
        }
        let audio = if sound {
            let codec = encoder::find(codec::Id::AAC).context("FFmpeg has no AAC encoder")?;
            let mut enc = codec::context::Context::new_with_codec(codec)
                .encoder()
                .audio()?;
            enc.set_rate(48_000);
            enc.set_channel_layout(ChannelLayout::STEREO);
            enc.set_format(Sample::F32(Type::Planar));
            enc.set_bit_rate(160_000);
            enc.set_time_base((1, 48_000));
            if global {
                enc.set_flags(codec::Flags::GLOBAL_HEADER);
            }
            let enc = enc.open_as(codec)?;
            let mut stream = output.add_stream(codec)?;
            stream.set_parameters(&enc);
            stream.set_time_base((1, 48_000));
            Some(enc)
        } else {
            None
        };
        output.write_header()?;
        let scaler = scaling::Context::get(
            Pixel::RGBA,
            width,
            height,
            Pixel::YUV420P,
            width,
            height,
            scaling::Flags::FAST_BILINEAR,
        )?;
        let audio_frame_size = audio
            .as_ref()
            .map(|a| a.frame_size() as usize)
            .unwrap_or(1024);
        Ok(Self {
            output,
            video,
            audio,
            scaler,
            rgba: frame::Video::new(Pixel::RGBA, width, height),
            yuv: frame::Video::new(Pixel::YUV420P, width, height),
            audio_frame_size,
            fps,
        })
    }
    pub fn video(&mut self, image: &RgbaImage, pts: i64) -> Result<()> {
        let stride = self.rgba.stride(0);
        let bytes = self.rgba.data_mut(0);
        for (source, dest) in image
            .as_raw()
            .chunks_exact(image.width() as usize * 4)
            .zip(bytes.chunks_mut(stride))
        {
            dest[..source.len()].copy_from_slice(source);
        }
        self.scaler.run(&self.rgba, &mut self.yuv)?;
        self.yuv.set_pts(Some(pts));
        self.video.send_frame(&self.yuv)?;
        self.drain(false)
    }
    pub fn audio(&mut self, samples: &[[f32; 2]], pts: i64) -> Result<()> {
        let Some(audio) = &mut self.audio else {
            return Ok(());
        };
        let mut frame = frame::Audio::new(
            Sample::F32(Type::Planar),
            samples.len(),
            ChannelLayout::STEREO,
        );
        frame.set_rate(48_000);
        frame.set_pts(Some(pts));
        for c in 0..2 {
            for (out, s) in frame.plane_mut::<f32>(c).iter_mut().zip(samples) {
                *out = s[c].clamp(-1.0, 1.0);
            }
        }
        audio.send_frame(&frame)?;
        self.drain(true)
    }
    fn drain(&mut self, audio: bool) -> Result<()> {
        let mut packet = Packet::empty();
        loop {
            let result = if audio {
                self.audio.as_mut().unwrap().receive_packet(&mut packet)
            } else {
                self.video.receive_packet(&mut packet)
            };
            match result {
                Ok(()) => {
                    let index = usize::from(audio);
                    packet.set_stream(index);
                    packet.rescale_ts(
                        (1, if audio { 48_000 } else { self.fps as i32 }),
                        self.output.stream(index).unwrap().time_base(),
                    );
                    packet.write_interleaved(&mut self.output)?;
                }
                Err(ff::Error::Eof) => break,
                Err(ff::Error::Other { errno }) if errno == ff::error::EAGAIN => break,
                Err(e) => bail!("Video/audio encoding failed: {e}"),
            }
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<()> {
        self.video.send_eof()?;
        self.drain(false)?;
        if let Some(a) = &mut self.audio {
            a.send_eof()?;
            self.drain(true)?;
        }
        self.output.write_trailer()?;
        Ok(())
    }
}
