use tokio::sync::mpsc;
use crate::AudioChannel;
use bytes::Bytes;

pub async decode_audio(sample_rate: usize, mut rx: mpsc::Receiver<Bytes>, 
                 channel_data_tx: Vec<mpsc::Sender<AudioChannel>>) -> anyhow::Result<()> 
{
    while let Some(data) = rx.recv().await {
        if data.len() % 4 != 0 {
            anyhow::bail!("Got partial sample: {} bytes", data.len());
        }
        
    }
}
