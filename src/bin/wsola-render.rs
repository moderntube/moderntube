//! Offline check of the WSOLA port: renders a 16-bit PCM WAV at a playback rate.
//! usage: wsola-render <in.wav> <out.wav> <rate>
use moderntube::playback::gst::wsola::Wsola;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (input, output, rate) = (&args[1], &args[2], args[3].parse::<f64>().expect("rate"));
    let bytes = std::fs::read(input).expect("read input");
    let (mut channels, mut sample_rate, mut data) = (0usize, 0u32, &bytes[0..0]);
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = &bytes[at + 8..(at + 8 + len).min(bytes.len())];
        if id == b"fmt " {
            channels = u16::from_le_bytes([body[2], body[3]]) as usize;
            sample_rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
        } else if id == b"data" {
            data = body;
        }
        at += 8 + len + (len & 1);
    }
    let samples: Vec<f32> = data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
    let mut w = Wsola::new(channels, sample_rate);
    let mut out = Vec::new();
    for chunk in samples.chunks(channels * 1024) {
        w.push_interleaved(chunk);
        w.process(rate, &mut out);
    }
    let pcm: Vec<u8> = out.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes()).collect();
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&(channels as u16).to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * channels as u32 * 2).to_le_bytes());
    wav.extend_from_slice(&(channels as u16 * 2).to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    std::fs::write(output, wav).expect("write output");
}
