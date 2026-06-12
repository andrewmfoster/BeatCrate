// Loudness analysis — pure-Rust BS.1770 integrated-loudness measurement, no
// external sidecar.
//
// We decode the file with symphonia and feed PCM frames to the `ebur128` crate,
// which implements the BS.1770 integrated-loudness measurement. The returned
// value is the gain offset (dB) that brings the track to -16 LUFS: positive =
// boost, negative = cut.

use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

const TARGET_LUFS: f64 = -16.0;

pub fn analyze_loudness(path: &Path) -> Result<f64, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe failed: {e}"))?;
    let mut format = probed.format;

    let track = format
        .default_track()
        .ok_or_else(|| "no default track".to_string())?;
    let track_id = track.id;
    let codec_params = track.codec_params.clone();

    let channels = codec_params
        .channels
        .ok_or_else(|| "unknown channel count".to_string())?
        .count() as u32;
    let sample_rate = codec_params
        .sample_rate
        .ok_or_else(|| "unknown sample rate".to_string())?;

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| format!("decoder init failed: {e}"))?;

    let mut ebu = ebur128::EbuR128::new(channels, sample_rate, ebur128::Mode::I)
        .map_err(|e| format!("ebur128 init failed: {e}"))?;

    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut buf_spec = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // End of stream — symphonia signals it as an UnexpectedEof IoError.
            Err(SymError::IoError(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(format!("read error: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                if sample_buf.is_none() {
                    let capacity = decoded.capacity() as u64;
                    buf_spec = Some(spec);
                    sample_buf = Some(SampleBuffer::<f32>::new(capacity, spec));
                } else if buf_spec != Some(spec) {
                    // symphonia guarantees a fixed spec per track; if that ever
                    // breaks, fail loudly instead of corrupting samples in
                    // copy_interleaved_ref() against a mis-sized buffer.
                    return Err(format!(
                        "sample spec changed mid-stream: {buf_spec:?} -> {spec:?}"
                    ));
                }
                if let Some(buf) = sample_buf.as_mut() {
                    buf.copy_interleaved_ref(decoded);
                    ebu.add_frames_f32(buf.samples())
                        .map_err(|e| format!("ebur128 add_frames failed: {e}"))?;
                }
            }
            // Skip a corrupt packet rather than aborting the whole file.
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decode error: {e}")),
        }
    }

    let lufs = ebu
        .loudness_global()
        .map_err(|e| format!("loudness_global failed: {e}"))?;
    if !lufs.is_finite() {
        // Digital silence (or near-silence) yields -inf; there is no numeric
        // LUFS to act on, so treat the track as a failure.
        return Err("non-finite integrated loudness".to_string());
    }

    Ok(TARGET_LUFS - lufs)
}
