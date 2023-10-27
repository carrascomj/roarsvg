use crate::LyonTranslationError;
use std::path::Path;
use usvg::{TreeWriting, XmlOptions};

/// Write to file, WASM aware.
///
/// WASM part adapted from [bevyengine/bevy#8455](/bevyengine/bevy/pull/8455).
pub fn to_file<P: AsRef<Path>>(tree: usvg::Tree, file_path: P) -> Result<(), LyonTranslationError> {
    // simply write string to path
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::io::Write;
        let mut output = std::fs::File::create::<P>(file_path)
            .map_err(|e| LyonTranslationError::IoWrite(Box::new(e)))?;
        write!(output, "{}", tree.to_string(&XmlOptions::default()))
            .map_err(|e| LyonTranslationError::IoWrite(Box::new(e)))?;
    }

    #[cfg(target_arch = "wasm32")]
    {
        let file_path = file_path.as_ref().to_owned();
        match (|| {
            use wasm_bindgen::{JsCast, JsValue};

            let blob = web_sys::Blob::new_with_str_sequence(&JsValue::from_str(
                tree.to_string(&XmlOptions::default()).as_str(),
            ))?;
            let url = web_sys::Url::create_object_url_with_blob(&blob)?;
            let window = web_sys::window().unwrap();
            let document = window.document().unwrap();
            let link = document.create_element("a")?;
            link.set_attribute("href", &url)?;
            link.set_attribute(
                "download",
                file_path
                    .file_name()
                    .and_then(|filename| filename.to_str())
                    .ok_or_else(|| JsValue::from_str("Invalid filename"))?,
            )?;
            let html_element = link.dyn_into::<web_sys::HtmlElement>()?;
            html_element.click();
            web_sys::Url::revoke_object_url(&url)?;
            Ok::<(), JsValue>(())
        })() {
            Err(e) => {
                return Err(LyonTranslationError::IoWrite(
                    e.as_string()
                        .unwrap_or("WASM write error!".to_string())
                        .into(),
                ))
            }
            _ => (),
        };
    }
    Ok(())
}
