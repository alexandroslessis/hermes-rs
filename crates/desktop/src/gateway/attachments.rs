//! Files and images attached in the composer (`hermes_core::attachments` holds the logic,
//! `crate::attachments` the shapes): `file.attach` copies a file into the session's folder,
//! referenced as `@file:` in the prompt; `image.attach*` hold images for the session's next prompt
//! (the page's path when it doesn't upload them with the prompt).

use std::sync::Arc;

use serde_json::{Value, json};

use super::Gateway;
use super::rpc::{ImageAttach, Reply, RpcError};

impl Gateway {
    pub(super) async fn file_attach(self: &Arc<Self>, params: Value) -> Reply {
        let id = super::session_id(&params)?;
        let name = params["name"].as_str().unwrap_or_default().to_string();
        let source = crate::attachments::attach_source(&params);
        // Reads the session and copies the file: off the async workers.
        self.blocking(move |g| {
            g.core
                .attach_file(&id, &name, source)
                .map(crate::attachments::attached)
                .map_err(RpcError::invalid)
        })
        .await?
    }

    pub(super) async fn image_attach(self: &Arc<Self>, p: ImageAttach) -> Reply {
        let id = p.session.id()?;
        let path = p.path;
        let checked = path.clone();
        let found = self
            .blocking(move |_| std::path::Path::new(&checked).is_file())
            .await?;
        if !found {
            return Err(RpcError::invalid(format!("image not found: {path}")));
        }
        if !crate::attachments::is_image(std::path::Path::new(&path)) {
            return Err(RpcError::invalid(format!("unsupported image: {path}")));
        }
        self.hold_image(&id, path)
    }

    pub(super) async fn image_attach_bytes(self: &Arc<Self>, p: ImageAttach) -> Reply {
        let id = p.session.id()?;
        // Decodes and saves the picture: off the async workers.
        let (dir, raw) = (self.data_dir.clone(), p.raw.clone());
        let (status, saved) = self
            .blocking(move |_| crate::attachments::image_from_base64(&dir, &raw))
            .await?;
        if status != 200 {
            let detail = saved["detail"]
                .as_str()
                .unwrap_or("The image wasn't saved.");
            return Err(RpcError::invalid(detail));
        }
        let path = saved["path"].as_str().unwrap_or_default().to_string();
        self.hold_image(&id, path)
    }

    /// Held by the session's actor (`PageState::images`) until the next prompt.
    fn hold_image(&self, id: &str, path: String) -> Reply {
        let count = self.core.hold_image(id, &path).map_err(RpcError::server)?;
        let name = std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        Ok(
            json!({"attached": true, "path": path, "name": name, "count": count,
                  "text": format!("[User attached image: {name}]")}),
        )
    }

    pub(super) fn image_detach(&self, p: ImageAttach) -> Reply {
        let id = p.session.id()?;
        let (detached, count) = self
            .core
            .detach_image(&id, &p.path)
            .map_err(RpcError::server)?;
        Ok(json!({"detached": detached, "count": count}))
    }
}
