use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_source::AudioSourceOptions;

fn main() {
    let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 100);
    println!("livekit {} (webrtc audio source at {} Hz)", livekit::SDK_VERSION, source.sample_rate());
    println!(
        "sherpa-onnx {} ({}), onnxruntime {}",
        sherpa_onnx::version(),
        sherpa_onnx::git_sha1(),
        sherpa_onnx::onnxruntime_version()
    );
}
