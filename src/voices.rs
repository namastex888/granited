//! Stored voices for `/v1/audio/speech`: one reference clip (WAV and its exact transcript) per mood,
//! under `state_dir/voices/<id>/` on the device that recorded them. Directories are 0700 and files
//! 0600; every write is temp file, fsync, rename. A request naming a voice gets the mood's clip sent
//! inline as `references`, so workers and peers never keep a copy.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Clip limits, the same as the Qwen3-TTS worker's own check.
const MIN_SECONDS: f64 = 1.0;
const MAX_SECONDS: f64 = 30.0;
const MAX_TEXT_CHARS: usize = 500;
const META: &str = "voice.json";

#[derive(Debug, PartialEq)]
pub enum VoiceError {
    NotFound,
    Exists,
    NoReferences,
    Invalid(String),
    Io(String),
}

impl VoiceError {
    pub fn code(&self) -> &'static str {
        match self {
            VoiceError::NotFound => "voice_not_found",
            VoiceError::Exists => "voice_exists",
            VoiceError::NoReferences => "voice_has_no_references",
            VoiceError::Invalid(_) => "invalid_schema",
            VoiceError::Io(_) => "voice_store_error",
        }
    }

    pub fn message(&self) -> String {
        match self {
            VoiceError::NotFound => "no stored voice has that id".into(),
            VoiceError::Exists => "a voice with that id already exists".into(),
            VoiceError::NoReferences => "the voice has no reference clip yet".into(),
            VoiceError::Invalid(m) | VoiceError::Io(m) => m.clone(),
        }
    }
}

impl From<std::io::Error> for VoiceError {
    fn from(e: std::io::Error) -> Self {
        VoiceError::Io(e.kind().to_string())
    }
}

/// A voice id or a mood: lowercase, starting with a letter or digit, at most 32 characters.
pub fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

/// What a request resolved to: the mood spoken and its reference, ready to send inline.
pub struct Resolved {
    pub mood_used: String,
    pub reference: Value,
    pub ref_hash: String,
    pub temperature: Option<f64>,
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(state_dir: &str) -> Store {
        Store {
            root: Path::new(state_dir).join("voices"),
        }
    }

    fn dir(&self, id: &str) -> Result<PathBuf, VoiceError> {
        if !valid_name(id) {
            return Err(VoiceError::Invalid(
                "voice_id must be [a-z0-9][a-z0-9_-]{0,31}".into(),
            ));
        }
        Ok(self.root.join(id))
    }

    fn meta(&self, id: &str) -> Result<Value, VoiceError> {
        let bytes = match std::fs::read(self.dir(id)?.join(META)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(VoiceError::NotFound),
            Err(e) => return Err(e.into()),
        };
        serde_json::from_slice(&bytes)
            .map_err(|_| VoiceError::Io("voice.json is unreadable".into()))
    }

    fn write_meta(&self, id: &str, meta: &Value) -> Result<(), VoiceError> {
        write_file(&self.dir(id)?.join(META), meta.to_string().as_bytes())
    }

    pub fn create(&self, id: &str, name: &str) -> Result<Value, VoiceError> {
        let dir = self.dir(id)?;
        if name.chars().count() > 64 {
            return Err(VoiceError::Invalid("name is at most 64 characters".into()));
        }
        make_dir(&self.root)?;
        match std::fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(VoiceError::Exists);
            }
            Err(e) => return Err(e.into()),
        }
        set_mode(&dir, 0o700)?;
        let meta = json!({"voice_id": id, "name": name, "personal": true, "default_mood": null,
            "temperature": null, "references": {}});
        self.write_meta(id, &meta)?;
        Ok(meta)
    }

    /// Stores one mood's clip after checking it: a WAV of 1 to 30 s and a transcript of 1 to 500
    /// characters. Replaces that mood's earlier clip.
    pub fn put_reference(
        &self,
        id: &str,
        mood: &str,
        audio: &[u8],
        text: &str,
    ) -> Result<Value, VoiceError> {
        let mut meta = self.meta(id)?;
        if !valid_name(mood) {
            return Err(VoiceError::Invalid(
                "mood must be [a-z0-9][a-z0-9_-]{0,31}".into(),
            ));
        }
        let text = text.trim();
        if text.is_empty() || text.chars().count() > MAX_TEXT_CHARS {
            return Err(VoiceError::Invalid(format!(
                "text must be the clip's exact transcript, 1 to {MAX_TEXT_CHARS} characters"
            )));
        }
        let mut wav = crate::activity::Wav::default();
        wav.feed(audio);
        let seconds = wav
            .seconds()
            .map_err(|_| VoiceError::Invalid("audio must be a PCM WAV file".into()))?;
        if !(MIN_SECONDS..=MAX_SECONDS).contains(&seconds) {
            return Err(VoiceError::Invalid(format!(
                "a reference clip must be {MIN_SECONDS} to {MAX_SECONDS} seconds long"
            )));
        }
        let dir = self.dir(id)?;
        write_file(&dir.join(format!("{mood}.wav")), audio)?;
        write_file(&dir.join(format!("{mood}.txt")), text.as_bytes())?;
        let entry = json!({"ref_hash": hash(audio), "seconds": (seconds * 100.0).round() / 100.0});
        meta["references"][mood] = entry.clone();
        self.write_meta(id, &meta)?;
        Ok(
            json!({"voice_id": id, "mood": mood, "ref_hash": entry["ref_hash"], "seconds": entry["seconds"]}),
        )
    }

    /// `default_mood` (null, or a mood name) and `temperature` (null, or 0 to 2).
    pub fn patch(&self, id: &str, changes: &Value) -> Result<Value, VoiceError> {
        let mut meta = self.meta(id)?;
        let Some(changes) = changes.as_object() else {
            return Err(VoiceError::Invalid("body must be an object".into()));
        };
        for (key, value) in changes {
            match key.as_str() {
                "default_mood" if value.is_null() || value.as_str().is_some_and(valid_name) => {}
                "temperature"
                    if value.is_null()
                        || value.as_f64().is_some_and(|t| (0.0..=2.0).contains(&t)) => {}
                "name" if value.as_str().is_some_and(|n| n.chars().count() <= 64) => {}
                _ => {
                    return Err(VoiceError::Invalid(format!(
                        "{key}: default_mood (a mood or null), temperature (0 to 2 or null) or name"
                    )));
                }
            }
            meta[key] = value.clone();
        }
        self.write_meta(id, &meta)?;
        Ok(meta)
    }

    pub fn get(&self, id: &str) -> Result<Value, VoiceError> {
        self.meta(id)
    }

    pub fn list(&self) -> Result<Value, VoiceError> {
        let mut voices = Vec::new();
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(json!({"voices": []}));
            }
            Err(e) => return Err(e.into()),
        };
        for entry in entries.flatten() {
            let id = entry.file_name().to_string_lossy().into_owned();
            if valid_name(&id)
                && let Ok(meta) = self.meta(&id)
            {
                voices.push(meta);
            }
        }
        voices.sort_by(|a, b| a["voice_id"].as_str().cmp(&b["voice_id"].as_str()));
        Ok(json!({"voices": voices}))
    }

    pub fn delete(&self, id: &str) -> Result<(), VoiceError> {
        let dir = self.dir(id)?;
        self.meta(id)?;
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    /// The mood to speak and its clip: the requested mood; with none and a question, `question`;
    /// then the voice's `default_mood`; then `neutral`; then any stored mood, so an unknown mood is
    /// never an error.
    pub fn resolve(
        &self,
        id: &str,
        mood: Option<&str>,
        input: &str,
    ) -> Result<Resolved, VoiceError> {
        let meta = self.meta(id)?;
        let refs = meta["references"].as_object().cloned().unwrap_or_default();
        let question = (mood.is_none() && input.trim_end().ends_with('?')).then_some("question");
        let chain = [
            mood,
            question,
            meta["default_mood"].as_str(),
            Some("neutral"),
        ];
        let chosen = chain
            .into_iter()
            .flatten()
            .find(|m| refs.contains_key(*m))
            .map(str::to_owned)
            .or_else(|| refs.keys().next().cloned())
            .ok_or(VoiceError::NoReferences)?;
        let dir = self.dir(id)?;
        let audio = std::fs::read(dir.join(format!("{chosen}.wav")))?;
        let text = std::fs::read_to_string(dir.join(format!("{chosen}.txt")))?;
        use base64::Engine;
        Ok(Resolved {
            reference: json!({"audio_base64": base64::engine::general_purpose::STANDARD.encode(&audio), "text": text}),
            ref_hash: hash(&audio),
            mood_used: chosen,
            temperature: meta["temperature"].as_f64(),
        })
    }
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn make_dir(dir: &Path) -> Result<(), VoiceError> {
    std::fs::create_dir_all(dir)?;
    set_mode(dir, 0o700)
}

fn set_mode(path: &Path, mode: u32) -> Result<(), VoiceError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

/// Temp file, fsync, rename: a crash leaves the old file or the new one, never half of one.
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), VoiceError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.tmp"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    set_mode(&temp, 0o600)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temp, path)?;
    if let Some(dir) = path.parent() {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> String {
        let dir =
            std::env::temp_dir().join(format!("granited-voices-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    /// A mono 16-bit 24 kHz WAV of `seconds`, filled with `level`.
    fn wav(seconds: f64, level: i16) -> Vec<u8> {
        let samples = (seconds * 24000.0) as u32;
        let data = samples * 2;
        let mut out = b"RIFF".to_vec();
        out.extend((36 + data).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend(24000u32.to_le_bytes());
        out.extend(48000u32.to_le_bytes());
        out.extend(2u16.to_le_bytes());
        out.extend(16u16.to_le_bytes());
        out.extend(b"data");
        out.extend(data.to_le_bytes());
        for _ in 0..samples {
            out.extend(level.to_le_bytes());
        }
        out
    }

    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn names_are_lowercase_and_short() {
        assert!(valid_name("mira") && valid_name("unfold-v1_pt") && valid_name("9"));
        for bad in ["", "Mira", "-x", "_x", "a/b", "..", "a b", &"x".repeat(33)] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn a_voice_is_private_on_disk_and_listed() {
        let dir = temp_dir("private");
        let store = Store::new(&dir);
        store.create("mira", "Mira").unwrap();
        assert_eq!(store.create("mira", "again"), Err(VoiceError::Exists));
        let put = store
            .put_reference("mira", "joy", &wav(2.0, 100), " Que bom! ")
            .unwrap();
        assert_eq!(put["seconds"], 2.0);
        let root = Path::new(&dir).join("voices");
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&root.join("mira")), 0o700);
        for file in ["voice.json", "joy.wav", "joy.txt"] {
            assert_eq!(mode(&root.join("mira").join(file)), 0o600, "{file}");
        }
        let listed = store.list().unwrap();
        assert_eq!(
            listed["voices"][0]["references"]["joy"]["ref_hash"],
            put["ref_hash"]
        );
        assert!(
            std::fs::read_dir(root.join("mira"))
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().ends_with(".tmp"))
        );
        store.delete("mira").unwrap();
        assert_eq!(store.get("mira"), Err(VoiceError::NotFound));
    }

    #[test]
    fn a_clip_must_be_a_wav_of_one_to_thirty_seconds_with_its_transcript() {
        let dir = temp_dir("clip");
        let store = Store::new(&dir);
        store.create("v", "").unwrap();
        for (audio, text) in [
            (wav(0.5, 1), "short"),
            (wav(31.0, 1), "long"),
            (b"not a wav".to_vec(), "text"),
            (wav(2.0, 1), "  "),
        ] {
            assert!(matches!(
                store.put_reference("v", "neutral", &audio, text),
                Err(VoiceError::Invalid(_))
            ));
        }
        assert!(matches!(
            store.put_reference("v", "Joy", &wav(2.0, 1), "t"),
            Err(VoiceError::Invalid(_))
        ));
        assert_eq!(
            store
                .put_reference("nobody", "joy", &wav(2.0, 1), "t")
                .err(),
            Some(VoiceError::NotFound)
        );
        assert!(matches!(
            store.create("../x", ""),
            Err(VoiceError::Invalid(_))
        ));
    }

    #[test]
    fn moods_fall_back_to_question_then_default_then_neutral_then_any() {
        let dir = temp_dir("moods");
        let store = Store::new(&dir);
        store.create("v", "").unwrap();
        assert!(matches!(
            store.resolve("v", None, "hi"),
            Err(VoiceError::NoReferences)
        ));
        store
            .put_reference("v", "calm", &wav(2.0, 3), "calm")
            .unwrap();
        // Only calm stored: any request speaks calm, never an error.
        assert_eq!(
            store.resolve("v", Some("joy"), "hi").unwrap().mood_used,
            "calm"
        );
        store
            .put_reference("v", "neutral", &wav(2.0, 1), "neutral")
            .unwrap();
        store
            .put_reference("v", "joy", &wav(2.0, 2), "joy")
            .unwrap();
        store
            .put_reference("v", "question", &wav(2.0, 4), "question")
            .unwrap();
        let used = |mood, input| store.resolve("v", mood, input).unwrap().mood_used;
        assert_eq!(used(Some("joy"), "Oi!"), "joy");
        assert_eq!(used(Some("sad"), "Oi!"), "neutral");
        assert_eq!(used(None, "Tudo bem? "), "question");
        // An explicit mood wins over the question rule.
        assert_eq!(used(Some("joy"), "Tudo bem?"), "joy");
        assert_eq!(used(None, "Oi."), "neutral");
        store
            .patch("v", &json!({"default_mood": "calm", "temperature": 1.0}))
            .unwrap();
        assert_eq!(used(None, "Oi."), "calm");
        assert_eq!(used(Some("sad"), "Oi."), "calm");
        let r = store.resolve("v", Some("joy"), "x").unwrap();
        assert_eq!(
            (r.reference["text"].as_str(), r.temperature),
            (Some("joy"), Some(1.0))
        );
        assert!(store.patch("v", &json!({"temperature": 5})).is_err());
        assert!(store.patch("v", &json!({"voice_id": "w"})).is_err());
    }
}
