# Streamer Template

This is a place to play around with some project layouts for audio streaming
servers that provide bidirectional streaming interface to some CPU bound model.

Currently very WIP. If you come back later this might be more fleshed out!

## Motivation 

In world there are APIs where you stream audio or some continuous data into
them and insights about said data are streamed out. I've had cause to do this
a lot in my work and have people with less domain experience work on these
systems as well. Often the audio processing is also computationally intensive
such as speech AI.

So with this project I wanted to create something laid out in a clean manner
which demonstrates numerous patterns and ways of designing such APIs in a way
that they're testable, maintainable and hopefully performant.

## The Patterns

In my background there's a few decisions to make. For processing the data via a
model we have the decison on whether we:

1. Process all of the incoming audio
2. Detect segments of interest and process them (VAD/energy filtering)

For the first options there's also a choice on whether we can process segments 
concurrently or if the result from one segment needs to be applied to the future
segment for various reasons i.e. smoothing/hiding seams generative outputs from
the audio.

Enumerating these patterns and representing them all in the code is a WIP.
Currently, I process everything and assume no relationship between utterances.
DayJob™ may open source some packages soon that I can reuse for some things like
filtering.

## What's Included

So far a server and a client to stream audio into it.
The server just returns a count of bytes in the segment but I may add some
more interesting non-filler functionality. The server also adds a blocking
delay because neural network inferences are blocking and it can serve as a
way of modelling how this impacts the performance of the async runtime.

Opentelemetry support is also included with trace propagation via websockets!
This is tricky because javascript doesn't let you set HTTP headers with
websocket requests so one approach APIs I've used have gone with is including
a trace-id in the first message sent over the websocket. I have replicated
this and handled the faffy-pain of trace propagation.

Tokio task metrics are now included for the main blocks in the streaming
pipeline, as well as other metrics in a Prometheus compatible endpoint.

## Running

For simple running I provide a client and a server. You should be able to run
the server with just:

```
cargo run --release
```

For the client you can run:

```
cargo run --release --bin client -- -i <WAVE FILE>
```

This will run as fast as possible via the VAD segmented API. If you want to
split to the version that splits the audio into equal chunks and puts all
audio through the model then:

```
cargo run --release --bin client -- -i <WAVE FILE> --addr ws://localhost:8080/api/v1/simple
```

Add `--real-time` to get the client to throttle chunk sending to match real-time
and `--interim-results` to get partial results.

For a more thorough server deployment look at the docker-compose and you'll
get goodies like prometheus and opentelemetry for metrics and traces.

## The Write-up

Look in the doc folder and you'll find the current published blog posts as well
as any draft ones or ones held back because they're out of sequence. Anything
not yet published may change significantly and be in a draft state, I wouldn't
recommend reading it unless you're very desperate or fancy providing some feedback.

Current planned writeups:

1. System introduction ✔️
2. Streaming API design ✔️
3. Audio Decoding ✔️
4. The Model 🚧
5. Creating an Axum API 🚧
6. Metrics for Streaming APIs 🚧
7. Observability with Opentelemetry ❌
8. Batching to improve async performance 🚧
9. Testing Streaming Audio Systems ❌
10. Implementing a gRPC API ❌

More things may appear as we get further and further on and I see more things
where there's not a good existing knowledge base. I'm also open to suggestions
on topics.
