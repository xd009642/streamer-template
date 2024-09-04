use crate::api_types::AudioFormat;
use crate::AudioChannel;
use bytes::Bytes;
use rubato::{FftFixedIn, Resampler};
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

    const RESAMPLER_SIZE: usize = 2048;

    let resample_ratio = 16000.0 / audio_format.sample_rate as f64;

    trace!("Resampler ratio: {}", resample_ratio);

    let mut resampler = if audio_format.sample_rate != 16000 {
        Some(FftFixedIn::new(
            audio_format.sample_rate as usize,
            16000,
            RESAMPLER_SIZE,
            1024,
            audio_format.channels,
        )?)
    } else {
        None
    };

    let resample_trigger_len = audio_format.channels * RESAMPLER_SIZE;
    trace!("Resampler trigger length: {}", resample_trigger_len);

    let mut received_samples = 0;
    let mut sent_samples = 0;

    let mut current_buffer = vec![];
    while let Some(data) = rx.recv().await {
        // We could do the sample extraction and uninterleave the samples in one go. But if you're
        // using an existing library like ffmpeg (or maybe gstreamer) to decode and resample audio
        // you'll get the audio interleaved and have to split it out so you will get a similar
        // performance profile anyway.
        let mut samples = match (audio_format.bit_depth, audio_format.is_float) {
            (16, false) => i16::to_float_samples(&data),
            (32, true) => f32::to_float_samples(&data),
            (bd, float) => {
                anyhow::bail!("Unsupported format bit_depth: {} is_float: {}", bd, float)
            }
        }?;
        received_samples += samples.len();
        current_buffer.append(&mut samples);

        if current_buffer.len() >= resample_trigger_len || resampler.is_none() {
            let len = current_buffer.len();
            let capacity = RESAMPLER_SIZE.min(current_buffer.len() / audio_format.channels);
            let mut channels = vec![Vec::with_capacity(RESAMPLER_SIZE); audio_format.channels];
            for (chan, data) in (0..channel_data_tx.len())
                .cycle()
                .zip(current_buffer.drain(..(capacity * audio_format.channels)))
            {
                channels[chan].push(data);
            }

            let mut channels = if let Some(resampler) = resampler.as_mut() {
                resampler.process(&channels, None)?
            } else {
                channels
            };

            for (i, (data, sink)) in channels.drain(..).zip(&channel_data_tx).enumerate() {
                //trace!("Emitting {} samples for channel {}", data.len(), i);
                sent_samples += data.len();
                sink.send(data.into()).await?;
            }
        }
    }
    if !current_buffer.is_empty() {
        trace!(
            "Sent out {} expected output {}",
            sent_samples,
            received_samples as f64 * resample_ratio
        );
        let new_len = if (sent_samples as f64) < received_samples as f64 * resample_ratio {
            let per_channel_sample = ((received_samples as f64 * resample_ratio)
                / audio_format.channels as f64)
                .round() as usize;
            Some(per_channel_sample - sent_samples)
        } else {
            None
        };
        let mut channels = vec![
            Vec::with_capacity(current_buffer.len() / audio_format.channels);
            audio_format.channels
        ];
        for (chan, data) in (0..channel_data_tx.len())
            .cycle()
            .zip(current_buffer.drain(..))
        {
            channels[chan].push(data);
        }
        let mut channels = if let Some(resampler) = resampler.as_mut() {
            resampler.process_partial(Some(&channels), None)?
        } else {
            channels
        };
        for (i, (mut data, sink)) in channels.drain(..).zip(&channel_data_tx).enumerate() {
            if let Some(new_len) = new_len {
                trace!(
                    "Downsizing to avoid trailing silence to {} bytes from {}",
                    new_len,
                    data.len()
                );
                data.resize(new_len, 0.0);
            }
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
    use approx::assert_abs_diff_eq;
    use bytes::{Buf, BytesMut};
    use dasp::{signal, Signal};
    use futures::stream::{FuturesOrdered, StreamExt};
    use std::{fs, path::Path};
    use tracing_test::traced_test;

    fn write_wav(path: &str, samples: &Vec<Vec<f32>>) {
        let spec = hound::WavSpec {
            channels: samples.len() as _,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        let samples_len = samples.iter().map(|x| x.len()).min().unwrap_or_default();
        for i in 0..samples_len {
            for c in 0..samples.len() {
                writer
                    .write_sample((samples[c][i] * i16::MAX as f32) as i16)
                    .unwrap();
            }
        }
        writer.finalize().unwrap();
    }

    /// Given an input audio format, the bytes for this audio, a chunk size to stream the bytes
    /// into the encoder and an expected output run the audio through the decoding pipeline and
    /// compare it.
    async fn test_audio(
        format: AudioFormat,
        mut input: BytesMut,
        chunk_size: usize,
        expected: Vec<Vec<f32>>,
        output_name: &str,
    ) {
        let (bytes_tx, bytes_rx) = mpsc::channel(8);

        let mut output_channels = vec![];
        let mut incoming_samples = FuturesOrdered::new();
        for _ in 0..format.channels {
            let (sample_tx, mut sample_rx) = mpsc::channel::<AudioChannel>(8);
            output_channels.push(sample_tx);
            incoming_samples.push_back(async move {
                let mut resampled: Vec<f32> = vec![];
                while let Some(samples) = sample_rx.recv().await {
                    resampled.extend_from_slice(&samples);
                }
                resampled
            });
        }

        let decoder = tokio::spawn(decode_audio(format, bytes_rx, output_channels));

        let handle = tokio::spawn(async move {
            while !input.is_empty() {
                let to_send = if input.remaining() > chunk_size {
                    input.split_to(chunk_size)
                } else {
                    input.split()
                };
                bytes_tx.send(to_send.freeze()).await.unwrap();
            }
        });

        let resampled = incoming_samples.collect::<Vec<_>>().await;

        let expected_name = format!("{}_expected.wav", output_name);
        let actual_name = format!("{}_actual.wav", output_name);
        write_wav(&expected_name, &expected);
        write_wav(&actual_name, &resampled);

        decoder.await.unwrap().unwrap();
        handle.await.unwrap();

        // Save our files for debugging

        for (channel_index, (expected_channel, actual_channel)) in
            expected.iter().zip(resampled.iter()).enumerate()
        {
            assert_eq!(expected_channel.len(), actual_channel.len());
            for (sample_index, (expected, actual)) in expected_channel
                .iter()
                .zip(actual_channel.iter())
                .enumerate()
            {
                assert_abs_diff_eq!(expected, actual, epsilon = 0.1); // This would be a 5% error
            }
        }

        let _ = fs::remove_file(&expected_name);
        let _ = fs::remove_file(&actual_name);
    }

    /// Here we're going to create a 2s sine wave at our desired sample rate. We will then send it
    /// to the transcoding and make sure we get it back as expected! This test will be in s16 as
    /// that is usually the most common sample format for most applications.
    #[tokio::test]
    #[traced_test]
    async fn pass_through_s16_audio() {
        let format = AudioFormat {
            sample_rate: 16000,
            channels: 1,
            bit_depth: 16,
            is_float: false,
        };

        let expected_output = signal::rate(16000.0)
            .const_hz(1600.0)
            .sine()
            .take(32000)
            .map(|x| x as f32)
            .collect::<Vec<f32>>();

        let input = expected_output
            .iter()
            .flat_map(|x| ((*x * i16::MAX as f32) as i16).to_le_bytes())
            .collect::<BytesMut>();

        test_audio(
            format,
            input,
            300,
            vec![expected_output],
            "pass_through_s16",
        )
        .await;
    }

    /// Here we're going to create a 2s sine wave at our desired sample rate. We will then send it
    /// to the transcoding and make sure we get it back as expected! This test will be in f32
    /// to ensure our only other format works as expected!
    #[tokio::test]
    #[traced_test]
    async fn pass_through_f32_audio() {
        let format = AudioFormat {
            sample_rate: 16000,
            channels: 1,
            bit_depth: 32,
            is_float: true,
        };

        let expected_output = signal::rate(16000.0)
            .const_hz(1600.0)
            .sine()
            .take(32000)
            .map(|x| x as f32)
            .collect::<Vec<f32>>();

        let input = expected_output
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect::<BytesMut>();

        test_audio(
            format,
            input,
            300,
            vec![expected_output],
            "pass_through_f32",
        )
        .await;
    }

    #[tokio::test]
    #[traced_test]
    async fn upsample_s16_audio() {
        let format = AudioFormat {
            sample_rate: 8000,
            channels: 1,
            bit_depth: 16,
            is_float: false,
        };

        let expected_output = signal::rate(16000.0)
            .const_hz(800.0)
            .sine()
            .take(64000)
            .map(|x| x as f32)
            .collect::<Vec<f32>>();

        let input = signal::rate(8000.0)
            .const_hz(800.0)
            .sine()
            .take(32000)
            .map(|x| x as f32)
            .flat_map(|x| ((x * i16::MAX as f32) as i16).to_le_bytes())
            .collect::<BytesMut>();

        test_audio(format, input, 300, vec![expected_output], "upsample_s16").await;
    }

    #[tokio::test]
    #[traced_test]
    async fn downsample_f32_audio() {
        let format = AudioFormat {
            sample_rate: 32000,
            channels: 1,
            bit_depth: 16,
            is_float: false,
        };

        // Input is 32khz and 64000 samples (so 2s)
        // Output is 16000 32000 samples (so 2s again)

        let expected_output = signal::rate(16000.0)
            .const_hz(800.0)
            .sine()
            .take(32000)
            .map(|x| x as f32)
            .collect::<Vec<f32>>();

        let input = signal::rate(32000.0)
            .const_hz(800.0)
            .sine()
            .take(64000)
            .flat_map(|x| (x as f32).to_le_bytes())
            .collect::<BytesMut>();

        test_audio(format, input, 300, vec![expected_output], "downsample_s16").await;
    }
}
