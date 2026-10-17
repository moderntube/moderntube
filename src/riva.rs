//! NVIDIA Riva offline ASR over NVCF gRPC: a unary HTTP/2 POST through reqwest, with a
//! hand-written message subset of `nvidia-riva/common` `riva_asr.proto` (MIT).

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use prost::Message;

const RECOGNIZE_PATH: &str = "/nvidia.riva.asr.RivaSpeechRecognition/Recognize";
const OGGOPUS: i32 = 4;
const SAMPLE_RATE: i32 = 16_000;
/// gRPC `RESOURCE_EXHAUSTED`: NVCF's trial rate limit.
const GRPC_RESOURCE_EXHAUSTED: u32 = 8;

#[derive(Clone, PartialEq, Message)]
pub struct RecognitionConfig {
    #[prost(int32, tag = "1")]
    pub encoding: i32,
    #[prost(int32, tag = "2")]
    pub sample_rate_hertz: i32,
    #[prost(string, tag = "3")]
    pub language_code: String,
    #[prost(int32, tag = "4")]
    pub max_alternatives: i32,
    #[prost(int32, tag = "7")]
    pub audio_channel_count: i32,
    #[prost(bool, tag = "8")]
    pub enable_word_time_offsets: bool,
    #[prost(bool, tag = "11")]
    pub enable_automatic_punctuation: bool,
}

#[derive(Clone, PartialEq, Message)]
pub struct RecognizeRequest {
    #[prost(message, optional, tag = "1")]
    pub config: Option<RecognitionConfig>,
    #[prost(bytes = "vec", tag = "2")]
    pub audio: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
pub struct WordInfo {
    #[prost(int32, tag = "1")]
    pub start_time: i32,
    #[prost(int32, tag = "2")]
    pub end_time: i32,
    #[prost(string, tag = "3")]
    pub word: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct SpeechRecognitionAlternative {
    #[prost(string, tag = "1")]
    pub transcript: String,
    #[prost(message, repeated, tag = "3")]
    pub words: Vec<WordInfo>,
}

#[derive(Clone, PartialEq, Message)]
pub struct SpeechRecognitionResult {
    #[prost(message, repeated, tag = "1")]
    pub alternatives: Vec<SpeechRecognitionAlternative>,
}

#[derive(Clone, PartialEq, Message)]
pub struct RecognizeResponse {
    #[prost(message, repeated, tag = "1")]
    pub results: Vec<SpeechRecognitionResult>,
}

#[derive(Debug)]
pub enum RivaError {
    RateLimited,
    Other(String),
}

impl std::fmt::Display for RivaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RivaError::RateLimited => f.write_str("rate-limited"),
            RivaError::Other(e) => f.write_str(e),
        }
    }
}

/// Length-prefixed gRPC message frame: 1 byte compressed flag, 4 bytes BE length.
pub fn frame(msg: &impl Message) -> Vec<u8> {
    let body = msg.encode_to_vec();
    let mut out = Vec::with_capacity(5 + body.len());
    out.push(0);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

pub fn unframe(buf: &[u8]) -> Result<&[u8], String> {
    if buf.len() < 5 {
        return Err(format!("short gRPC body ({} bytes)", buf.len()));
    }
    if buf[0] != 0 {
        return Err("compressed gRPC message".to_string());
    }
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    buf.get(5..5 + len).ok_or_else(|| "truncated gRPC message".to_string())
}

/// One `(start_ms, text)` per result, offsets relative to the start of the audio.
pub fn segments(resp: &RecognizeResponse) -> Vec<(i64, String)> {
    resp.results
        .iter()
        .filter_map(|r| r.alternatives.first())
        .filter_map(|a| {
            let text = a.transcript.trim();
            let start = a.words.first()?.start_time as i64;
            (!text.is_empty()).then(|| (start, text.to_string()))
        })
        .collect()
}

/// Transcode to 16 kHz mono Ogg/Opus. `audiorate` fills timestamp gaps (held DVR stretches)
/// with silence: Riva misreads Opus across a granule jump, and its offsets stay in stream time.
pub async fn to_ogg_opus(input: Vec<u8>) -> Result<Vec<u8>, String> {
    tokio::task::spawn_blocking(move || transcode_blocking(input))
        .await
        .map_err(|e| e.to_string())?
}

fn transcode_blocking(input: Vec<u8>) -> Result<Vec<u8>, String> {
    gst::init().map_err(|e| e.to_string())?;
    let pipeline = gst::parse::launch(&format!(
        "appsrc name=src ! decodebin ! audioconvert ! audioresample ! audiorate ! \
         audio/x-raw,rate={SAMPLE_RATE},channels=1 ! opusenc bitrate=32000 ! oggmux ! \
         appsink name=sink sync=false"
    ))
    .map_err(|e| e.to_string())?
    .downcast::<gst::Pipeline>()
    .map_err(|_| "not a pipeline".to_string())?;
    let src = pipeline
        .by_name("src")
        .and_then(|e| e.downcast::<gst_app::AppSrc>().ok())
        .ok_or("no appsrc")?;
    let sink = pipeline
        .by_name("sink")
        .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
        .ok_or("no appsink")?;
    pipeline.set_state(gst::State::Playing).map_err(|e| e.to_string())?;
    let pushed = src
        .push_buffer(gst::Buffer::from_mut_slice(input))
        .and_then(|_| src.end_of_stream());
    let mut out = Vec::new();
    let result = match pushed {
        Err(e) => Err(format!("appsrc: {e:?}")),
        Ok(_) => loop {
            match sink.pull_sample() {
                Ok(sample) => {
                    if let Some(map) = sample.buffer().and_then(|b| b.map_readable().ok()) {
                        out.extend_from_slice(&map);
                    }
                }
                Err(_) => break Ok(()),
            }
        },
    };
    let bus_err = pipeline.bus().and_then(|bus| {
        bus.pop_filtered(&[gst::MessageType::Error]).and_then(|m| match m.view() {
            gst::MessageView::Error(e) => Some(e.error().to_string()),
            _ => None,
        })
    });
    let _ = pipeline.set_state(gst::State::Null);
    result?;
    match bus_err {
        Some(e) => Err(format!("transcode: {e}")),
        None if out.is_empty() => Err("transcode produced no audio".to_string()),
        None => Ok(out),
    }
}

/// One offline `Recognize` call. `client` must speak HTTP/2 (see [`client`]).
pub async fn recognize(
    client: &reqwest::Client,
    base_url: &str,
    function_id: &str,
    key: &str,
    ogg_opus: Vec<u8>,
) -> Result<RecognizeResponse, RivaError> {
    let req = RecognizeRequest {
        config: Some(RecognitionConfig {
            encoding: OGGOPUS,
            sample_rate_hertz: SAMPLE_RATE,
            language_code: "en-US".to_string(),
            max_alternatives: 1,
            audio_channel_count: 1,
            enable_word_time_offsets: true,
            enable_automatic_punctuation: true,
        }),
        audio: ogg_opus,
    };
    let resp = client
        .post(format!("{}{RECOGNIZE_PATH}", base_url.trim_end_matches('/')))
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .header("function-id", function_id)
        .bearer_auth(key)
        .body(frame(&req))
        .send()
        .await
        .map_err(|e| RivaError::Other(e.to_string()))?;
    let status = resp.status();
    let grpc_status = resp
        .headers()
        .get("grpc-status")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok());
    let grpc_message = resp
        .headers()
        .get("grpc-message")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if status.as_u16() == 429 || grpc_status == Some(GRPC_RESOURCE_EXHAUSTED) {
        return Err(RivaError::RateLimited);
    }
    if let Some(code) = grpc_status.filter(|&c| c != 0) {
        return Err(RivaError::Other(format!("gRPC status {code}: {grpc_message}")));
    }
    if !status.is_success() {
        return Err(RivaError::Other(format!("HTTP {status}")));
    }
    let body = resp.bytes().await.map_err(|e| RivaError::Other(e.to_string()))?;
    let msg = unframe(&body).map_err(RivaError::Other)?;
    RecognizeResponse::decode(msg).map_err(|e| RivaError::Other(e.to_string()))
}

/// An HTTP/2-only client: gRPC does not run over HTTP/1.1.
pub fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .http2_prior_knowledge()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(start: i32) -> WordInfo {
        WordInfo { start_time: start, end_time: start + 100, word: "w".into() }
    }

    fn alt(text: &str, words: Vec<WordInfo>) -> SpeechRecognitionResult {
        SpeechRecognitionResult {
            alternatives: vec![SpeechRecognitionAlternative { transcript: text.into(), words }],
        }
    }

    #[test]
    fn grpc_frame_round_trips_a_response() {
        let resp = RecognizeResponse { results: vec![alt("hello there", vec![word(240)])] };
        let framed = frame(&resp);
        assert_eq!(framed[0], 0);
        assert_eq!(u32::from_be_bytes(framed[1..5].try_into().unwrap()) as usize, framed.len() - 5);
        let back = RecognizeResponse::decode(unframe(&framed).unwrap()).unwrap();
        assert_eq!(back, resp);
    }

    #[test]
    fn a_truncated_grpc_body_is_an_error() {
        assert!(unframe(&[0, 0, 0]).is_err());
        assert!(unframe(&[0, 0, 0, 0, 9, 1, 2]).is_err());
    }

    #[test]
    fn transcript_segments_start_at_the_first_word_and_skip_empty_results() {
        let resp = RecognizeResponse {
            results: vec![
                alt(" But let's get a bucket of XP. ", vec![word(240), word(900)]),
                alt("   ", vec![word(5000)]),
                alt("no words", vec![]),
                alt("Wait, I have an idea.", vec![word(17_440)]),
            ],
        };
        assert_eq!(
            segments(&resp),
            vec![(240, "But let's get a bucket of XP.".to_string()), (17_440, "Wait, I have an idea.".to_string())]
        );
    }

    fn wav_tone(secs: u32) -> Vec<u8> {
        let rate = 44_100u32;
        let samples: Vec<i16> = (0..rate * secs)
            .map(|i| ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 8000.0) as i16)
            .collect();
        let data_len = samples.len() as u32 * 2;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        for v in [16u32, 1 | (1 << 16), rate, rate * 2, 2 | (16 << 16)] {
            wav.extend_from_slice(&v.to_le_bytes());
        }
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        samples.iter().for_each(|s| wav.extend_from_slice(&s.to_le_bytes()));
        wav
    }

    #[tokio::test]
    async fn audio_is_transcoded_to_a_16khz_ogg_opus_stream() {
        let ogg = to_ogg_opus(wav_tone(2)).await.unwrap();
        assert_eq!(&ogg[..4], b"OggS");
        let head = ogg.windows(8).position(|w| w == b"OpusHead").expect("OpusHead");
        assert_eq!(u32::from_le_bytes(ogg[head + 12..head + 16].try_into().unwrap()), 16_000);
    }

}
