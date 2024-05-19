use crate::AudioChannel;
use bytes::Bytes;
use tokio::sync::mpsc;

pub async fn decode_audio(
    sample_rate: usize,
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
