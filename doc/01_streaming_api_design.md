# Designing an Audio Streaming API

_This is part of a series on creating a streaming audio service in Rust, with
services such as live transcription in mind. I recommend you read previous
entries if this is the first one you've stumbled upon!_

Our previous entry just initially introduced the system, now it's time to look
outside-in and see what decisions we have to make in terms of our API. Our
system is a bidirectional streaming system - meaning that data is streamed
both from client to server and server to client. This prevents us from doing
a standard REST API so typically you'll find people use one of two approaches:

1. Websockets
2. gRPC

There's also a third option of using raw HTTP/2 streams which the AWS
transcribe service does. But that definitely feels like a spicier option that's
more unfriendly to people not using any of the languages the official
clients are provided in. There's a fourth option of abusing WebRTC and I won't
say anymore about that. Any other options let me know, this feels fairly
comprehensive.

for the purpose of this we'll start by looking at the first two, their pros and
cons and then the act of designing the API.

## Websockets

Websockets work pretty much everywhere which is definitely a plus in their
favour. You establish a HTTP connection and then upgrade it to a raw TCP
socket. This means you get all the pros and cons of raw TCP sockets

1. Easy to use
2. Completely untyped

There's also an extra negatives if you expect a browser based frontend
to interact with your service. You can't set HTTP headers when opening a 
websocket!

This has implications if you expect to use the Authorization header to
validate a user's allowed to interact with your service.

## gRPC 

gRPC works over HTTP/2 meaning you can only use it where you have HTTP/2, which
while it may be a lot of places it's not everywhere. I've previously had
issues with some cloud providers (Huawei). You also won't be able to do
bidirectional streaming from the browser so using things like gRPC web or
one of the auto-proxy generation solutions to expose it to the browser won't
work.

However, it does offer some form of typing for your API even if it is Protobuf.
Additionally, there's a gRPC ecosystem you can interact with which can give
some benefits of it's own.

## Which to Choose?

So you don't actually need to choose just one! You can choose to do both an
multiplex your requests. Later on we'll do this, but for now we'll choose to
do websockets just to keep things simpler and have our API usable from more
contexts.

## Designing the API

Now it's time to think a bit about what we want to do and see what design falls
out as a result. We already know we're sending audio in but how are we sending
it in? 

Audio files?

Audio files are simple for the client which is definitely a plus. However, it's
complicated our end and not every file is streamable. If you've tried to stream
audio files before you may have encountered an issue where there is trailer
data at the end of the file which ffmpeg et al insist on reading before they'll
decode any samples. This then ends up removing the streaming part of our streaming
service! Also, things like microphones don't tend to output audio in a file. So
what's our next choice?

PCM Data!

Pulse Code Modulation (PCM) data is audio data in it's simplest form. A list of
samples (typically little endian), where channels are interleaved. We don't need
to know anything except for the number of channels, the bits per sample and the
sample rate and we can perfectly reconstruct the audio file. This is also how
Wave files work after their headers (which contain all of that data and some more).

The only issue with PCM data is if people have something like an MP3 file they
may struggle to use your API, but you can always offer a non-streaming API that
handles file decoding if that becomes necessary. 

Now we could enforce to our users that they only provide data in our desired
sample format, channel count and sample rate. But that will go down like a lead
balloon and you'll soon find yourself with product owners knocking down your
door to demand a useable API.

With this in mind lets add a starting message to our API. We can also use this
to propagate other information i.e. any parameters the users can tweak, tracing
information and any possible options on the response. Let's start with something
like:

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StartMessage {
    /// Format information for the audio samples
    pub format: AudioFormat,
}

/// Describes the PCM samples coming in. 
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

After, this is sent I'll assume we'll be sending the audio data itself. This
will just be done via raw bytes because any audio streaming APIs that take
base64 data in json are an afront to all that is holy. You have a TCP socket
make use of it!

When audio is coming in we're going to be choosing segments from it to run
our model on. This means we have some way of identifying the start and end
of a segment. But when our client is done they may want any final inferences
and if we haven't detected the end of the segment they need a way to signal to
us to stop. Then we can force an end of segment and send the final results back.
So for this I define a stop message:

```rust
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StopMessage {
    /// Whether the server should close the websocket connection after returning final results.
    pub disconnect: bool,
}
```

And then to make deserializing the messages over websocket easier we'll use a
Rust enum like so:

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum RequestMessage {
    Start(StartMessage),
    Stop(StopMessage),
}
```

So as a first pass this is the input stream:

```
[start] [data]... [stop]
```

Of course the client can also instantly disconnect if they don't care about the
final result. And after a stop we can potentially keep the connection open and
send another start (though the server may want to timeout idle connections like
that).

With the inputs initially decided, lets work out the outputs!

The easiest possible output is just sending back the raw model outputs whatever,
they are. Some model outputs don't require any post-processing and for them that
can work. However, it'll only work if the user is able to work out the start and
end times automatically based on the index of the message in the stream. 

An example of a service that could do that might be a voice activity detection
service where each output is one frame of data and the frame size is known by
the user. We'll do a "simple" API in this project to cover that usecase.

But, often segments may be different sizes and then it's useful to know the start
and end times of the segment - we'll refer to this as the "segmemnted API". 
We also, want a way to report back any errors. With these initial requirements I
ended up settling on something like this:

```rust
/// If we're processing segments of audio we want people to be able to apply the results to the
/// actual segments!
#[derive(Debug, Serialize, Deserialize)]
pub struct SegmentOutput {
    /// Start time of the segment in seconds
    pub start_time: f32,
    /// End time of the segment in seconds
    pub end_time: f32,
    /// The output from our ML model
    #[serde(flatten)]
    pub output: model::Output,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Data(model::Output),
    Segment(SegmentOutput),
    Error(String),
}
```

Of course we could just use segment for both the simple and the segmented
routes (and this may end up happening in the actual code). But this works
reasonably well as an initial API with minimal complications.

## But Wait There's More!

Oh of course, there had to be more right! Well let's get into it, the next
subsections will be related to those facets of an API and I'll show how our
request/response times change due to it.

### Interim Results

Looking at the segmented API, we may want our service to be reporting
observations at regular intervals not just when a segment start and end
have been identified. These are known as interim or partial results.

You can see these in effect in live transcriptions on things like Google
Meet when works change in the transcript. The transcription service will
be returning output for every X seconds added onto the segment. Because of
this words may be cut off prematurely, or a lack of context causes errors
in the transcript which hopefully are fixed when the model runs on the entire
segment.

Not every user wants this, if you're not displaying things to a user or trying
to do speculative processing before the full segment is processed they're 
not very useful. So we want it to be an opt-in. With this in mind the start
message becomes:

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StartMessage {
    /// Format information for the audio samples
    pub format: AudioFormat,
    /// Whether interim results should be provided. An alternative API to this would be to specify
    /// the interval at which interim results are returned.
    #[serde(default)]
    pub interim_results: bool,
}
```

There is a potential option to allow the user to set the interval to return
results at, but this is a potentially dangerous API decision. Partial inferences
take time and doing them too frequently could cause your real-time system to
become not real-time as it falls further and further behind. In which case you
have to decide if you want to return an API error for too-small values or round
them up to an acceptable value.

Naturally, it's now important for our results to indicate if they're a final
result that won't change or if they're an interim result that may be corrected.
With this in mind our `SegmentOutput` turns into:

```rust
/// If we're processing segments of audio we want people to be able to apply the results to the
/// actual segments!
#[derive(Debug, Serialize, Deserialize)]
pub struct SegmentOutput {
    /// Start time of the segment in seconds
    pub start_time: f32,
    /// End time of the segment in seconds
    pub end_time: f32,
    /// Some APIs may do the inverse check of "is_partial" where the last request in an utterance
    /// would be `false`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_final: Option<bool>,
    /// The output from our ML model
    #[serde(flatten)]
    pub output: model::Output,
}
```

### Events

For the purpose of debugging or richer interactions you may want to expose
events where some processing has started. A semi-common example I've seen in
Speech To Text systems is an event when speech start and speech end are
detected. This means that before we have any transcripts back we know the user
is speaking and then can trigger some animation in the frontend or prepare for
incoming data.

These can also be quite useful for debugging. Did we get no output because our
model returned nothing or because we haven't detected any speech has started?

For events in our future API we'll just be implementing them for the segmented
API and not the simple one. Adding in these events our output type becomes:

```rust
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Data(model::Output),
    Segment(SegmentOutput),
    Error(String),
    Active { time: f32 },
    Inactive { time: f32 },
}
```

## Conclusion 

From this we've looked at some of the API decisions and started to define some
basic types for our API that are generally applicable to any bidirectional
streaming audio APIs. In future installments we'll actually start writing up
some code to make this works and breaking apart the things which make it all
come together.
