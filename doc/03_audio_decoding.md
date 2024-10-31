## Audio Decoding 

In the last entry it was decided that the audio coming in would be PCM data
with a specified sample rate, channel count and data type. This is quite
helpful as we don't need to think about audio codecs and setting up decoders.
There is one thing we have to think of though, converting from the input sample
rate to our models desired sample rate. 

Models typically only work on one sample rate. Rarely, you might see a model
take in 2 and do some basic resampling logic at the input. But in these cases
giving it the right sample rate first will cause the inference code to do less 
work so it's still preferable.

I also said previously we'd be using the actor model, if you're not familiar
there's this blogpost by Alice Rhyl
[actors with tokio](https://ryhl.io/blog/actors-with-tokio/).

The input to the function will be a channel sending the encoding audio data,
we'll already have the format info when we call it to keep things simpler. The
output will be decoded samples sent down an audio-channel specific channel.

I'll do my best to try and avoid audio-channel vs tokio channel confusion,
but please leave feedback if it's ambiguous anywhere. The terminology overlap
is a real pain at times!

```rust
use tokio::sync::mpsc;
use crate::api_types::AudioFormat;
use bytes::Bytes;

pub async fn decode_audio(
    audio_format: AudioFormat,
    mut rx: mpsc::Receiver<Bytes>,
    channel_data_tx: Vec<mpsc::Sender<Vec<f32>>>,
) -> anyhow::Result<()> {
    todo!()
}
```

To recap the `AudioFormat` type is:

```rust
/// Describes the PCM samples coming in. I could have gone for an enum instead of bit_depth +
/// is_float but I only really plan on doing f32, s16 and ignoring everything else including
/// compression schemes like mulaw etc. This may change in future.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AudioFormat {
    /// Number of channels in the audio
    pub channels: usize,
    /// Sample rate of the audio
    pub sample_rate: usize,
    /// Number of bits per sample
    pub bit_depth: u16,
    /// Whether audio uses floating point samples
    pub is_float: bool,
}
```

There are raw bytes coming in, potential of any width and we have floats coming
out. Better think about decoding those samples. For this I'll make a `Sample`
trait, it's likely an abstraction overkill but there is a potential to macro
generate this code later and reduce the amount of code we write and it's not
harmful.

```rust
trait Sample: Copy {
    fn to_float_samples(data: &[u8]) -> anyhow::Result<Vec<f32>>;

    fn to_float(self) -> f32;
}
```

Implementing this for `i16` would look like:

```rust
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
```

To do this for `f32`, we just change the `2` to `4`, `i16` to `f32` and make
`to_float` return self. We can also remove the `map(|x| x.to_float())`. The
copy-paste-ability of this code does make generating it automatically in the
future potentially very nice.

To make this better you could have `to_float_samples` take in an output
buffer and reuse allocations. That can be an exercise for the reader (or a
future post). But generally speaking, this part of the code is always going
to be lightning fast compared to the model code so we'll save our optimisation
efforts to things that side.

Initially, we'll just error out on any formats that aren't the correct sample
rate. This will help me keep things simpler initially. So with some initial
error checking we have everything we need to do a first implementation:

```rust
pub async fn decode_audio(
    audio_format: AudioFormat,
    mut rx: mpsc::Receiver<Bytes>,
    channel_data_tx: Vec<mpsc::Sender<Vec<f32>>>,
) -> anyhow::Result<()> {
    if channel_data_tx.is_empty() {
        anyhow::bail!("No output sinks for channel data");
    }

    if audio_format.sample_rate != MODEL_SAMPLE_RATE {
        // No need to be more specific in message, we'll rewrite this later
        anyhow::bail!("Invalid sample rate provided");
    }    
    
    let mut received_samples = 0;
    let mut sent_samples = 0;

    let mut current_buffer = vec![];
    while let Some(data) = rx.recv().await {
        let mut samples = match (audio_format.bit_depth, audio_format.is_float) {
            (16, false) => i16::to_float_samples(&data),
            (32, true) => f32::to_float_samples(&data),
            (bd, float) => {
                anyhow::bail!("Unsupported format bit_depth: {} is_float: {}", bd, float)
            }
        }?;
        received_samples += samples.len();
        current_buffer.append(&mut samples);
        
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
        for (data, sink) in channels.drain(..).zip(&channel_data_tx) {
            sink.send(data).await?;
        }
    }
}
```

Looking at this there's probably a few questions. 

* Could we handle interleaving in the decoding side instead of an extra loop?
* Could we also not await on the channel send and fire off more futures quicker?

The answer to both of these is yes. 

However, considering the interleaving if you're using an existing library
like ffmpeg or maybe gstreamer to decode samples you often get your data
interleaved and then have to do this extra step anyways. For services with a
streaming and non-streaming mode where the non-streaming accepts any potential
file format having solid reusable audio code for both is pretty nice and that's
how it exists at my Day Job so that's what's replicated here.

For the second point this might be done in the future, but for now we have the
"good enough" mindset. At least there's potentially easy optimisations we can
work on in future. Also, in the short-term we know our audio is coming through
in the correct ordering and once we have more testing infrastructure in place
we can work on harder things.

### Resampling Audio

We'll be implementing our audio resampling via the [rubato](https://crates.io/crates/rubato) 
crate. It seems to be the most thorough crate and provides a number of algorithms
and parameters to tweak them. And what is software if not the desire for an
abundance of knobs to twiddle?

The resamplers in Rubato are either synchronous or asynchronous, but this is
another collision of terminology between domains. An asynchronous resampler
allows you to adjust the resampling ratio while the resampler is running. This
could be useful for certain codecs or when using some sort of audio
protocol that might adjust sample rate based on the bandwidth available.

We're not going to change the sample rate so we don't really have to worry
about this. But if you do work on an application where this can happen you
should think about your maximum possible sample rate and the algorithm used.
Changing the sample rate in certain ways can either be completely fine or
change the aliasing effects in resampling so a wrong decision can increase
the noise in the output!
