# Batching for Speed

To recap, with our streaming system we have messages moving deeper into the
system via actors:

1. Client streaming bytes into our API route via websocket
2. Messages being extracted and audio streamed to audio decoding
3. Decoded samples streamed to inference task and processed by model

And then from the reverse going from the model to the client:

1. Inference results from the model sent back as API messages
3. API messages encoded and sent back to client via websocket

When dealing with channels and actor models there are a few different
ways the systems can behave that determine our approach in the design for
maximum throughput. For each channel we're concerned with:

1. Consistency of messages - is it bursty or constant
2. Amount of data - how much data are we sending and split into how many messages
3. Speed of messages - how fast are messages being produced

So for the messages we send it's time to classify our
channels into these categories. I'll do this first and then justify each
one.

| Channel | Bursty/Constant | Amount | Speed | 
| ------- | --------------- | ------ | ----- | 
| Client->API | Likely constant | Depends on audio processed and client chunking. But potentially a large amount | Depends (likely fast) |
| Audio bytes -> Decoder | Same as above | Same as above | Same as above |
| Decoder -> Model | Same as above* | Same as above* | Same as above* | 
| Model -> API | Constant | Smaller messages | Slow | 


The API to websocket doesn't matter so much as it should just be running
roughly as fast as the model outputs given we're just json encoding and
sending. 

Now the first three stages the client pretty much sets the speed. As we're
output float samples to the model the data will likely increase in size (most
audio tends to be s16 samples). However if we're receiving >1 channel of audio
then the message sizes will stay the same but we'll fan out to two inference
runners. For this we'll just assume single channel though.

Where things change is the model stage. The model is a slower process and 
while it's processing audio samples are queuing up and the channel is
potentially filling up. If the decoder->model channel completely fills up it
can also ripple backwards and cause everything else to stop and cause a
bottleneck.

Things get more complicated in real life. In ffmpeg based transcoding I've
observed the ffmpeg packets that are output are in ~40ms chunks, so a client
chunk of 100ms would turn into 2.5x decoded audio packets going into the model.
This means in terms of channel size we should consider a bigger channel for the
decoder into the model to handle the splitting of larger chunks into smaller 
decoded packets.

But even doing that we're still going to hit lag. And in comes batching. 

## Batching Things

In the toy model I add an artificial delay to simulate a costly inference.
Assuming that we can process all data and we're not working on fixed chunk
sizes into the model. If we can pull off more audio before inference without
delaying we can reduce the number of inferences, reduce actors waiting for a
channel to empty before they can progress and this should significantly speed
things up.

With this in mind the task to pull out decoded samples turns from this:

```rust
while let Some(decoded_audio) = audio_rx.recv().await {
    // run inference on decoded_audio
}
```

To this:

```rust
let mut messages = Vec::with_capacity(audio_rx.max_capacity());
while audio_rx.recv_many(&mut messages, audio_rx.max_capacity()).await > 0 {
    let mut decoded_audio = messages.remove(0);
    for message in messages.drain(..) {
        decoded_audio.append(&mut message);
    }

    // Run inference on decoded_audio
}
```

Implementing something similar to this on one of our internal services lead to
the latency of a streaming request dropping to 50% of what it was before the PR.

## Caveats/Notes/Other-Unnamed-Section

So I mentioned fixed chunk sizes before, this can still speed them up, provided
your model accepts batch inputs you can do multiple batches at the same time and
get a list of outputs. The only place that wouldn't work is if data derived from the
previous segment result is used in the input to the model as well.

Larger inferences aren't necessarily slower as well. A lot of models have a
fixed size context window and if data is below that window size it will be
padded. Additionally, with a GPU often transferring more data less often is a
better decision performance wise.

Reducing the number of inferences required also leads to less contention over
resources like GPU and reduces the risk of OOM failures on the less reliable
part of pipelines (overcommitting on memory and no fallible allocations are a
nightmare on shared GPU resources).

## Conclusion

This is a draft post of a work in progress educational project. If you've found
this well done I guess? There's still a lot of TODOs in terms of benchmarking,
diagrams, other things explaining the system in more detail and just general
development. Hopefully it's useful to you as is but you should check again
when I actually post about this more publicly.
