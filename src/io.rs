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
        #[derive(Debug)]
        struct WASMError(&'static str);

        match (|| {
            let file_path = file_path.as_ref().to_owned();
            use wasm_bindgen::{JsCast, JsValue};
            let svg = tree.to_string(&XmlOptions::default());
            web_sys::console::log_1(&svg.clone().into());
            let blob = web_sys::Blob::new_with_str_sequence(&js_sys::Array::from_iter(
                std::iter::once(JsValue::from_str(svg.as_str())),
            ))
            .map_err(|_| WASMError("error writing blob"))?;
            let url = web_sys::Url::create_object_url_with_blob(&blob)
                .map_err(|_| WASMError("error writing url"))?;
            let window = web_sys::window().unwrap();
            let document = window.document().unwrap();
            let link = document
                .create_element("a")
                .map_err(|_| WASMError("error creating <a>"))?;
            link.set_attribute("href", &url)
                .map_err(|_| WASMError("error creating <href>"))?;
            link.set_attribute(
                "download",
                file_path
                    .file_name()
                    .and_then(|filename| filename.to_str())
                    .ok_or_else(|| WASMError("Invalid filename"))?,
            )
            .map_err(|_| WASMError("Invalid filename"))?;
            let html_element = link
                .dyn_into::<web_sys::HtmlElement>()
                .map_err(|_| WASMError("error creating <html>"))?;
            html_element.click();
            web_sys::Url::revoke_object_url(&url).map_err(|_| WASMError("Error revoking url"))?;
            Ok::<(), WASMError>(())
        })() {
            Err(e) => return Err(LyonTranslationError::IoWrite(format!("{:?}", e).into())),
            _ => (),
        };
    }
    Ok(())
}
