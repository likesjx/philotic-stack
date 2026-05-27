use anyhow::{Context, Result};
use async_trait::async_trait;
use datasource::controller::{DatasourceProvider, DatasourceTask, ProviderOutput, TaskKind};
use datasource::runtime::{DatasourceGuestConfig, run_datasource_controller};
use serde_json::Value;
use std::sync::Arc;
use tracing::info;

fn guest_id() -> String {
    std::env::var("PHILOTIC_VISION_GUEST_ID")
        .unwrap_or_else(|_| "vision-01".to_string())
}

fn python_path() -> String {
    std::env::var("PHILOTIC_VISION_PYTHON").unwrap_or_else(|_| "python3".to_string())
}

// ── ScriptProvider ────────────────────────────────────────────────────────────

struct ScriptProvider {
    python_path: String,
    script_path: Option<String>,
}

impl ScriptProvider {
    fn new(python_path: String, script_path: Option<String>) -> Self {
        Self {
            python_path,
            script_path,
        }
    }

    fn run_python(&self, task_kind: &str, params: &Value) -> Result<String> {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let script = self.script_path.as_deref().map(String::from).unwrap_or_else(|| {
            builtin_script()
        });

        let input = serde_json::json!({
            "task": task_kind,
            "params": params,
        })
        .to_string();

        let mut child = Command::new(&self.python_path)
            .args(["-c", &script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn Python vision subprocess")?;

        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(input.as_bytes())
                .context("failed to write to Python stdin")?;
        }

        let output = child
            .wait_with_output()
            .context("failed to wait on Python vision subprocess")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("vision script exited with error: {stderr}");
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

fn builtin_script() -> String {
    r#"
import sys, json

data = json.loads(sys.stdin.read())
task = data.get("task", "")
params = data.get("params", {})

def ocr(params):
    try:
        from PIL import Image
        import io, base64, urllib.request
        if "image_base64" in params:
            img_bytes = base64.b64decode(params["image_base64"])
            img = Image.open(io.BytesIO(img_bytes))
        elif "image_url" in params:
            with urllib.request.urlopen(params["image_url"]) as r:
                img = Image.open(io.BytesIO(r.read()))
        else:
            return {"error": "image.ocr requires image_url or image_base64"}
        try:
            import pytesseract
            text = pytesseract.image_to_string(img)
            return {"text": text.strip()}
        except ImportError:
            pass
        try:
            from transformers import pipeline
            pipe = pipeline("image-to-text")
            result = pipe(img)
            text = result[0]["generated_text"] if result else ""
            return {"text": text.strip()}
        except Exception as e:
            return {"error": f"OCR backend unavailable: {e}"}
    except Exception as e:
        return {"error": str(e)}

def ground(params):
    try:
        from PIL import Image
        import io, base64, urllib.request
        if "image_base64" in params:
            img_bytes = base64.b64decode(params["image_base64"])
            img = Image.open(io.BytesIO(img_bytes))
        elif "image_url" in params:
            with urllib.request.urlopen(params["image_url"]) as r:
                img = Image.open(io.BytesIO(r.read()))
        else:
            return {"error": "image.ground requires image_url or image_base64"}
        query = params.get("query", "")
        try:
            from transformers import pipeline
            pipe = pipeline("zero-shot-object-detection")
            results = pipe(img, candidate_labels=[query] if query else ["object"])
            return {"results": results}
        except Exception as e:
            return {"error": f"grounding backend unavailable: {e}"}
    except Exception as e:
        return {"error": str(e)}

if task == "image.ocr":
    out = ocr(params)
elif task == "image.ground":
    out = ground(params)
else:
    out = {"error": f"unsupported task: {task}"}

print(json.dumps(out))
"#
    .to_string()
}

#[async_trait]
impl DatasourceProvider for ScriptProvider {
    fn id(&self) -> &str {
        "script"
    }

    fn supports(&self, task: &DatasourceTask) -> bool {
        matches!(
            &task.kind,
            TaskKind::Custom(k) if k == "image.ocr" || k == "image.ground"
        )
    }

    async fn invoke(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let task_kind = task.kind.as_str();

        let result_str = tokio::task::spawn_blocking({
            let python_path = self.python_path.clone();
            let script_path = self.script_path.clone();
            let task_kind = task_kind.to_string();
            let params = task.parameters.clone();
            move || {
                let provider = ScriptProvider::new(python_path, script_path);
                provider.run_python(&task_kind, &params)
            }
        })
        .await
        .context("vision script blocking task panicked")?
        .context("vision script execution failed")?;

        let parsed: Value = serde_json::from_str(&result_str)
            .unwrap_or_else(|_| serde_json::json!({"text": result_str}));

        if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
            anyhow::bail!("vision script returned error: {err}");
        }

        Ok(ProviderOutput::ResultSet(parsed))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let id: &'static str = Box::leak(guest_id().into_boxed_str());
    let py = python_path();
    let script = std::env::var("PHILOTIC_VISION_SCRIPT").ok();

    info!(guest_id = id, python = %py, "model-controller-vision starting");

    let provider = Arc::new(ScriptProvider::new(py, script));

    run_datasource_controller(DatasourceGuestConfig {
        guest_id: id,
        role: "model-controller-vision",
        providers: Box::new(move || vec![provider.clone()]),
    })
    .await
    .context("model-controller-vision controller exited with error")
}
