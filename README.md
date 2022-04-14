# tracing-xray

Type definitions: https://docs.aws.amazon.com/xray/latest/devguide/xray-api-segmentdocuments.html
Sending: https://docs.aws.amazon.com/xray/latest/devguide/xray-api-sendingdata.html

## Listening on UDP port 2000
To listen on the same port as the X-Ray daemon would, you can use `netcat`
``` sh
sudo yum install nc
# Listen on port 2000
nc -ul 2000
```

## Testing tracing-xray + mini-redis locally
Prerequisites: You should have an AWS account already created.

In the AWS console, create a new IAM user, and give it the Permissions policy called `AWSXRayDaemonWriteAccess` (this policy already exists, you don't have to create it).

Grab the Access Key ID and Secret Access Key for your user. You will want to create a credentials file called ``~/.aws/credentials`:
```
[default]
aws_access_key_id = <id>
aws_secret_access_key = <key>
```

Download the xray daemon and run it (you can also run the Dockerized version if you prefer, full instructions here: https://docs.aws.amazon.com/xray/latest/devguide/xray-daemon-local.html)
``` sh
curl https://s3.us-east-2.amazonaws.com/aws-xray-assets.us-east-2/xray-daemon/aws-xray-daemon-linux-3.x.zip --output aws-xray-daemon-linux-3.x.zip

unzip aws-xray-daemon-linux-3.x.zip

chmod +x ./xray
```

Now, run xray with `./xray`. Use `-n` to set the region, i.e, `-n us-east-2`).

At this point, you will then need to:
- Clone https://github.com/jswrenn/tracing-xray
- Clone mini-redis (this branch https://github.com/jswrenn/mini-redis/tree/tracing-xray) adjacent to the tracing-xray folder
- Run `cargo run --bin mini-redis-server`
- In another terminal, run `redis-benchmark -t get,set -n 1` (you can increase the number to create more load)

You should see something like: `[Info] Successfully sent batch of 50 segments (0.024 seconds)`

In the AWS console, you can go to https://us-west-2.console.aws.amazon.com/cloudwatch/home?region=us-west-2#xray:traces/query to see your traces (make sure you are in the correct region)