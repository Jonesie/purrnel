//! One-shot camera frames for the `look` tool. The camera is never streamed:
//! a frame is grabbed only when the cat actually looks, and never while muted.

use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// Returns one JPEG. Skips the first frames so auto-exposure can settle.
pub async fn capture_jpeg(device: &str) -> Result<Vec<u8>, String> {
    let run = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-f", "v4l2", "-input_format", "mjpeg"])
        .args(["-video_size", "640x480", "-i", device])
        .args(["-vf", "select=gte(n\\,8)", "-frames:v", "1", "-q:v", "5"])
        .args(["-f", "image2pipe", "-c:v", "mjpeg", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .map_err(|_| "the camera took too long to answer".to_string())?
        .map_err(|e| format!("cannot run ffmpeg: {e}"))?;
    if out.stdout.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("no picture from the camera: {}", err.trim()));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    /// Needs a real webcam, so it only runs on request: `cargo test -- --ignored camera`.
    #[tokio::test]
    #[ignore]
    async fn grabs_a_jpeg_from_the_webcam() {
        let jpeg = super::capture_jpeg("/dev/video0").await.expect("a frame");
        assert_eq!(&jpeg[..2], &[0xff, 0xd8], "JPEG magic bytes");
        assert!(jpeg.len() > 5_000);
    }
}
