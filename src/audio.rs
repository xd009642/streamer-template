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
        if data.len() % 4 != 0 {
            anyhow::bail!("Got partial sample: {} bytes", data.len());
        }
        let mut channels = vec![
            Vec::with_capacity(data.len() / (4 * channel_data_tx.len()));
            channel_data_tx.len()
        ];
        for (chan, data) in (0..channel_data_tx.len()).cycle().zip(
            data.chunks(4)
                .map(|x| f32::from_le_bytes((&x[..4]).try_into().unwrap())),
        ) {
            channels[chan].push(data);
        }
        for (data, sink) in channels.drain(..).zip(&channel_data_tx) {
            sink.send(data.into()).await?;
        }
    }
    Ok(())
}
