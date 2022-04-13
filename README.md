# tracing-xray

Type definitions: https://docs.aws.amazon.com/xray/latest/devguide/xray-api-segmentdocuments.html
Sending: https://docs.aws.amazon.com/xray/latest/devguide/xray-api-sendingdata.html

## UDP
To listen on the same port as the X-Ray daemon would, you can use `netcat`
``` sh
sudo yum install nc
# Listen on port 2000
nc -ul 2000
```