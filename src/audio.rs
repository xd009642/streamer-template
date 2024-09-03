use crate::api_types::AudioFormat;
use crate::AudioChannel;
use bytes::Bytes;
use tokio::sync::mpsc;
use tracing::instrument;

/// So here we'd typically do more advanced things, namely:
///
/// 1. Convert from provided sample rate to desired sample rate
/// 2. Convert from provided sample format to desired sample format
///
/// But doing them often involves inclusion of tools like ffmpeg so this is removed in the aim of
/// keeping it simple!
#[instrument(skip(rx, channel_data_tx))]
pub async fn decode_audio(
    audio_format: AudioFormat,
    mut rx: mpsc::Receiver<Bytes>,
    channel_data_tx: Vec<mpsc::Sender<AudioChannel>>,
) -> anyhow::Result<()> {
    if channel_data_tx.is_empty() {
        anyhow::bail!("No output sinks for channel data");
    }
    while let Some(data) = rx.recv().await {
        // We could do the sample extraction and uninterleave the samples in one go. But if you're
        // using an existing library like ffmpeg (or maybe gstreamer) to decode and resample audio
        // you'll get the audio interleaved and have to split it out so you will get a similar
        // performance profile anyway.
        let samples = match (audio_format.bit_depth, audio_format.is_float) {
            (16, false) => i16::to_float_samples(&data),
            (32, true) => f32::to_float_samples(&data),
            (bd, float) => {
                anyhow::bail!("Unsupported format bit_depth: {} is_float: {}", bd, float)
            }
        }?;
        let mut channels =
            vec![Vec::with_capacity(samples.len() / channel_data_tx.len()); channel_data_tx.len()];
        for (chan, data) in (0..channel_data_tx.len()).cycle().zip(samples.iter()) {
            channels[chan].push(*data);
        }
        for (data, sink) in channels.drain(..).zip(&channel_data_tx) {
            sink.send(data.into()).await?;
        }
    }
    Ok(())
}

trait Sample: Copy {
    fn to_float_samples(data: &[u8]) -> anyhow::Result<Vec<f32>>;

    fn to_float(self) -> f32;
}

impl Sample for i16 {
    fn to_float_samples(data: &[u8]) -> anyhow::Result<Vec<f32>> {
        if data.len() % 2 != 0 {
            anyhow::bail!("Got a partial sample: {} bytes", data.len());
        }
        let samples = data
            .chunks(2)
            .map(|x| i16::from_le_bytes((&x[..2]).try_into().unwrap()))
            .map(|x| x.to_float())
            .collect();
        Ok(samples)
    }

    fn to_float(self) -> f32 {
        self as f32 / i16::MAX as f32
    }
}

impl Sample for f32 {
    fn to_float_samples(data: &[u8]) -> anyhow::Result<Vec<f32>> {
        if data.len() % 4 != 0 {
            anyhow::bail!("Got a partial sample: {} bytes", data.len());
        }
        let samples = data
            .chunks(4)
            .map(|x| f32::from_le_bytes((&x[..4]).try_into().unwrap()))
            .collect();
        Ok(samples)
    }

    fn to_float(self) -> f32 {
        self
    }
}
