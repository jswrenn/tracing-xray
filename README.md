# tracing-xray

This is a proof-of-concept tracing layer, that propagates [tracing] spans to [AWS X-Ray].

[tracing]: https://crates.io/crates/tracing
[AWS X-Ray]: https://aws.amazon.com/xray/

## Tutorial

### 1. Installing the X-Ray Tracing Layer
First, add `tracing`, `tracing_subscriber`, `tracing-xray` crates as dependencies:
```toml
tracing = "0.1.13"
tracing-subscriber = "0.3.11"
tracing-xray = { git = "https://github.com/jswrenn/tracing-xray.git" }
```

Then, add `tracing_xray::Layer` as a tracing layer:
```rust
#[tokio::main]
pub async fn main() -> mini_redis::Result<()> {
    use tracing_subscriber::{registry::Registry, prelude::*};
    
    let _subscriber = Registry::default()
        .with(tracing_xray::Layer::new("YOUR-SERVICE-NAME".to_owned()).await?)
        .init();
        
    ...
}
```

Finally, use the `#[tracing::instrument]` [macro] to trace key functions in your application. For a complete example, see [here](https://github.com/jswrenn/mini-redis/tree/tracing-xray).

[macro]: https://docs.rs/tracing/latest/tracing/attr.instrument.html

#### Emitting Segments
X-Ray [segments] are emitted for all tracing spans with an explicit `aws.xray.trace_id` field; e.g.:
```rust
#[instrument(
    skip(db), 
    fields(
        aws.xray.trace_id = &tracing_xray::trace_id::new().as_str(),
    ),
)]
async fn handle(db: &Db, request: Request) -> crate::Result<()> {
    ...
}
```

[segments]: https://docs.aws.amazon.com/xray/latest/devguide/xray-concepts.html#xray-concepts-segments


#### Emitting Subsegments
X-Ray [subsegments] are emitted for all tracing spans that have an ancestor with an explicit `aws.xray.trace_id` field.

[subsegments]: https://docs.aws.amazon.com/xray/latest/devguide/xray-concepts.html#xray-concepts-subsegments


#### Emitting Metadata and Annotations
The [fields] of tracing spans are translated into X-Ray [annotations and metadata]. To emit an X-Ray annotation, prefix your field's name with `aws.xray.annotations.`. All other fields (except for `aws.xray.trace_id`) are emitted as metadata.

[fields]: https://docs.rs/tracing/0.1.*/tracing/field/index.html
[annotations and metadata]: https://docs.aws.amazon.com/xray/latest/devguide/xray-concepts.html#xray-concepts-annotations


#### Testing the Layer
To test whether X-Ray (sub)segments are being emitted, use netcat to listen to UDP traffic on local port 2000 while your application is doing work; e.g.:
```
$ nc -ul 2000
{"format": "json", "version": 1}
{"name":"mini-redis-server","id":"0000000000000006","start_time":1650387854.3217046,"trace_id":"1-625eeb8e-1ea68f58158546959418a682","metadata":{"tracing.file":"src/server.rs","tracing.line":329},"annotations":{"tracing.name":"run","tracing.target":"mini_redis::server"},"in_progress":true}
{"format": "json", "version": 1}
{"name":"apply","id":"000000c000000007","start_time":1650387854.3320353,"trace_id":"1-625eeb8e-45b511399406edc3a114d091","parent_id":"0000004000000001","type":"subsegment","metadata":{"tracing.file":"src/cmd/set.rs","tracing.line":127},"annotations":{"tracing.target":"mini_redis::cmd::set"},"end_time":1650387854.3321207}
{"format": "json", "version": 1}
{"name":"apply","id":"000000c000000007","start_time":1650387854.3320353,"trace_id":"1-625eeb8e-45b511399406edc3a114d091","parent_id":"0000004000000001","type":"subsegment","metadata":{"tracing.file":"src/cmd/set.rs","tracing.line":127},"annotations":{"tracing.target":"mini_redis::cmd::set"},"in_progress":true}
{"format": "json", "version": 1}
{"name":"mini-redis-server","id":"0000004000000001","start_time":1650387854.3205793,"trace_id":"1-625eeb8e-45b511399406edc3a114d091","metadata":{"tracing.file":"src/server.rs","tracing.line":329},"annotations":{"tracing.name":"run","tracing.target":"mini_redis::server"},"end_time":1650387854.332266}
```

### 2. Forwarding Traces to X-Ray
**This assumes you have an AWS account already created.**

First, in the AWS console, create a new IAM user, and give it the Permissions policy called `AWSXRayDaemonWriteAccess` (this policy already exists, you don't have to create it).

Then, grab the Access Key ID and Secret Access Key for your user. You will want to create a credentials file called ``~/.aws/credentials`:
```
[default]
aws_access_key_id = <id>
aws_secret_access_key = <key>
```

Next, [download] the X-Ray daemon.

[download]:https://docs.aws.amazon.com/xray/latest/devguide/xray-daemon.html#xray-daemon-downloading

Finally, run the X-Ray daemon with `./xray`. Use `-n` to set the region, i.e, `-n us-east-2`). In the terminal running the X-Ray daemon, you should see something like:
```
[Info] Successfully sent batch of 50 segments (0.024 seconds)
```
In the AWS console, you can go to the [X-Ray web interface] to see your traces (make sure you are in the correct region).

[X-Ray web interface]: https://us-west-2.console.aws.amazon.com/cloudwatch/home?region=us-west-2#xray:traces/query

## Limitations
This crate is a proof-of-concept. It is not supported, nor production-ready. Its implementation is deliberately simple.
