//! cmd_serve — serve the interactive D3 graph explorer.

use std::net::TcpListener;
use std::path::Path;

pub fn cmd_serve(db: &Path, bind: &str, algorithm: &str) -> std::io::Result<()> {
    let listener = TcpListener::bind(bind)?;
    println!("Ariadne graph explorer listening on http://{}", bind);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(e) = super::http::handle_http(stream, db, algorithm) {
                    tracing::warn!("serve request failed: {}", e);
                }
            }
            Err(e) => tracing::warn!("serve connection failed: {}", e),
        }
    }
    Ok(())
}
