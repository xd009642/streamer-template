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

There's also a choice on whether we can process segments concurrently or if the
result from one segment needs to be applied to the future segment for various
reasons i.e. smoothing/hiding seams generative outputs from the audio.

Enumerating these patterns and representing them all in the code is a WIP.
Currently, I process everything and assume no relationship between utterances.
DayJob™ may open source some packages soon that I can reuse for some things like
filtering.

## What's Included

So far a server (pretty unconfigurable) and a client to stream audio into it.
The server just returns a count of bytes in the segment but I may add some
more interesting non-filler functionality. The server also adds a blocking
delay because neural network inferences are blocking and it can serve as a
way of modelling how this impacts the performance of the async runtime.

Opentelemetry support is also included with trace propagation via websockets!
This is tricky because javascript doesn't let you set HTTP headers with
websocket requests so one approach APIs I've used have gone with is including
a trace-id in the first message sent over the websocket. I have replicated
this and handled the faffy-pain of trace propagation.

## Future Work

Things like limiting based on resources, i.e. we don't want to consume all
GPU memory. Additionally, various metrics, testing strategies and all the
things you need to run this stuff with any real confidence in production.
Probably a big ol' writeup about it as well.

If I want to do more of the audio side I might look at pure rust audio
format conversion as that's often a component.
