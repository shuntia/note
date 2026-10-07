//! VOICEVOX CORE through its C API, loaded at run time from the release's `libvoicevox_core.so`.

use std::ffi::{c_char, CStr, CString};
use std::io::Cursor;
use std::path::Path;
use std::ptr;

use anyhow::{anyhow, bail, Context};
use libloading::{Library, Symbol};

#[repr(C)]
struct LoadOnnxruntimeOptions {
    filename: *const c_char,
}

#[repr(C)]
struct InitializeOptions {
    acceleration_mode: i32,
    cpu_num_threads: u16,
}

#[repr(C)]
struct LoadVoiceModelOptions {
    on_existing: i32,
}

#[repr(C)]
struct TtsOptions {
    enable_interrogative_upspeak: bool,
}

enum Opaque {}

const CPU: i32 = 1;
const OK: i32 = 0;

pub struct Voicevox {
    lib: Library,
    synthesizer: *mut Opaque,
    style: u32,
}

// The synthesizer is used from one thread at a time, behind the engines' lock.
unsafe impl Send for Voicevox {}

impl Voicevox {
    /// Loads `dir`'s lib/libvoicevox_core.so and lib/libvoicevox_onnxruntime.so.1.17.3, the
    /// OpenJTalk dictionary in dict/, and model.vvm, which holds `style`; on the CPU.
    pub fn load(dir: &Path, style: u32, threads: u16) -> anyhow::Result<Self> {
        let lib = unsafe { Library::new(dir.join("lib/libvoicevox_core.so")) }
            .context("loading libvoicevox_core.so")?;
        let ort = cstring(&dir.join("lib/libvoicevox_onnxruntime.so.1.17.3"))?;
        let dict = cstring(&dir.join("dict"))?;
        let model = cstring(&dir.join("model.vvm"))?;
        unsafe {
            let check = |code: i32, what: &str| -> anyhow::Result<()> {
                if code == OK {
                    return Ok(());
                }
                let message: Symbol<unsafe extern "C" fn(i32) -> *const c_char> =
                    lib.get(b"voicevox_error_result_to_message")?;
                bail!(
                    "{what}: {}",
                    CStr::from_ptr(message(code)).to_string_lossy()
                )
            };
            let load_ort: Symbol<
                unsafe extern "C" fn(LoadOnnxruntimeOptions, *mut *const Opaque) -> i32,
            > = lib.get(b"voicevox_onnxruntime_load_once")?;
            let mut onnxruntime = ptr::null();
            check(
                load_ort(
                    LoadOnnxruntimeOptions {
                        filename: ort.as_ptr(),
                    },
                    &mut onnxruntime,
                ),
                "loading onnxruntime",
            )?;

            let jtalk_new: Symbol<unsafe extern "C" fn(*const c_char, *mut *mut Opaque) -> i32> =
                lib.get(b"voicevox_open_jtalk_rc_new")?;
            let mut jtalk = ptr::null_mut();
            check(
                jtalk_new(dict.as_ptr(), &mut jtalk),
                "loading the OpenJTalk dictionary",
            )?;

            let synthesizer_new: Symbol<
                unsafe extern "C" fn(
                    *const Opaque,
                    *const Opaque,
                    InitializeOptions,
                    *mut *mut Opaque,
                ) -> i32,
            > = lib.get(b"voicevox_synthesizer_new")?;
            let mut synthesizer = ptr::null_mut();
            let options = InitializeOptions {
                acceleration_mode: CPU,
                cpu_num_threads: threads,
            };
            check(
                synthesizer_new(onnxruntime, jtalk, options, &mut synthesizer),
                "creating the synthesizer",
            )?;

            let open: Symbol<unsafe extern "C" fn(*const c_char, *mut *mut Opaque) -> i32> =
                lib.get(b"voicevox_voice_model_file_open")?;
            let mut file = ptr::null_mut();
            check(open(model.as_ptr(), &mut file), "opening the voice model")?;
            let load_model: Symbol<
                unsafe extern "C" fn(*const Opaque, *const Opaque, LoadVoiceModelOptions) -> i32,
            > = lib.get(b"voicevox_synthesizer_load_voice_model")?;
            let loaded = load_model(synthesizer, file, LoadVoiceModelOptions { on_existing: 0 });
            let close: Symbol<unsafe extern "C" fn(*mut Opaque)> =
                lib.get(b"voicevox_voice_model_file_delete")?;
            close(file);
            check(loaded, "loading the voice model")?;
            Ok(Voicevox {
                lib,
                synthesizer,
                style,
            })
        }
    }

    /// Mono samples and their sample rate.
    pub fn render(&mut self, text: &str) -> anyhow::Result<(Vec<f32>, u32)> {
        let text = CString::new(text)?;
        let wav = unsafe {
            let tts: Symbol<
                unsafe extern "C" fn(
                    *const Opaque,
                    *const c_char,
                    u32,
                    TtsOptions,
                    *mut usize,
                    *mut *mut u8,
                ) -> i32,
            > = self.lib.get(b"voicevox_synthesizer_tts")?;
            let free: Symbol<unsafe extern "C" fn(*mut u8)> = self.lib.get(b"voicevox_wav_free")?;
            let (mut len, mut data) = (0usize, ptr::null_mut());
            let code = tts(
                self.synthesizer,
                text.as_ptr(),
                self.style,
                TtsOptions {
                    enable_interrogative_upspeak: true,
                },
                &mut len,
                &mut data,
            );
            if code != OK {
                let message: Symbol<unsafe extern "C" fn(i32) -> *const c_char> =
                    self.lib.get(b"voicevox_error_result_to_message")?;
                return Err(anyhow!(
                    "VOICEVOX: {}",
                    CStr::from_ptr(message(code)).to_string_lossy()
                ));
            }
            let wav = std::slice::from_raw_parts(data, len).to_vec();
            free(data);
            wav
        };
        let mut reader = hound::WavReader::new(Cursor::new(wav))?;
        let rate = reader.spec().sample_rate;
        let samples = reader
            .samples::<i16>()
            .map(|s| s.map(|s| f32::from(s) / 32768.0))
            .collect::<Result<_, _>>()?;
        Ok((samples, rate))
    }
}

fn cstring(path: &Path) -> anyhow::Result<CString> {
    Ok(CString::new(path.to_str().context("a non-UTF-8 path")?)?)
}
