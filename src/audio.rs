use crate::api_types::AudioFormat;
use crate::AudioChannel;
use bytes::Bytes;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use tokio::sync::mpsc;
use tracing::{instrument, trace};

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

    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        oversampling_factor: 128,
        interpolation: SincInterpolationType::Quadratic,
        window: WindowFunction::Blackman,
    };

    let mut resampler = if audio_format.sample_rate != 16000 {
        Some(SincFixedIn::new(
            16000.0 / audio_format.sample_rate as f64,
            1.0,
            params,
            256,
            audio_format.channels,
        )?)
    } else {
        None
    };

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
        let mut channels = if let Some(resampler) = resampler.as_mut() {
            resampler.process(&channels, None)?
        } else {
            channels
        };
        for (data, sink) in channels.drain(..).zip(&channel_data_tx) {
            sink.send(data.into()).await?;
        }
    }
    trace!("Audio decoding finished with no issues");
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

#[cfg(test)]
mod tests {
    use super::*;
    use approx::ulps_eq;
    use bytes::{Buf, BytesMut};
    use dasp::{signal, Signal};
    use tracing_test::traced_test;

    /// Here we're going to create a 2s sine wave at our desired sample rate. We will then send it
    /// to the transcoding and make sure we get it back as expected!
    #[tokio::test]
    #[traced_test]
    async fn pass_through_audio() {
        let format = AudioFormat {
            sample_rate: 16000,
            channels: 1,
            bit_depth: 32,
            is_float: true,
        };

        let (sample_tx, mut sample_rx) = mpsc::channel(8);
        let (bytes_tx, bytes_rx) = mpsc::channel(8);

        let output_channels = vec![sample_tx];

        let decoder = tokio::spawn(decode_audio(format, bytes_rx, output_channels));

        let expected_output = signal::rate(16000.0)
            .const_hz(16000.0)
            .sine()
            .take(32000)
            .map(|x| x as f32)
            .collect::<Vec<f32>>();

        let mut input = expected_output
            .iter()
            .flat_map(|x| ((*x * i16::MAX as f32) as i16).to_le_bytes())
            .collect::<BytesMut>();

        let handle = tokio::spawn(async move {
            while !input.is_empty() {
                let to_send = if input.remaining() > 300 {
                    input.split_to(300)
                } else {
                    input.split()
                };
                bytes_tx.send(to_send.freeze()).await.unwrap();
            }
        });

        let mut resampled = vec![];
        while let Some(samples) = sample_rx.recv().await {
            resampled.extend_from_slice(&samples);
        }

        assert_eq!(expected_output.len(), resampled.len());

        for (expected, actual) in expected_output.iter().zip(resampled.iter()) {
            ulps_eq!(expected, actual);
        }

        handle.await.unwrap();
    }
}
