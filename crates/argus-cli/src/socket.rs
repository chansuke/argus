//! The host's Unix socket from the client side: `Hello` says who is calling, then each
//! request is one `argus_api::ClientFrame` per line, answered by `ServerFrame`s.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use argus_api::wire::Wire;
use argus_api::{ApiError, ClientFrame, ServerFrame, Transport};
use argus_json::Json;

pub(crate) struct SocketTransport {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u32,
}

fn lost(e: impl std::fmt::Display) -> ApiError {
    ApiError::Internal {
        what: format!("the connection to the host failed: {e}"),
    }
}

impl SocketTransport {
    /// Connects and sends `Hello { token, run }`. The host closes the connection on a
    /// `Hello` it does not accept, which the first request then reports.
    pub(crate) fn connect(
        path: &Path,
        token: Option<String>,
        run: Option<String>,
    ) -> std::io::Result<SocketTransport> {
        let stream = UnixStream::connect(path)?;
        let writer = stream.try_clone()?;
        let mut transport = SocketTransport {
            reader: BufReader::new(stream),
            writer,
            next_id: 1,
        };
        transport.send(&ClientFrame::Hello { token, run })?;
        Ok(transport)
    }

    fn send(&mut self, frame: &ClientFrame) -> std::io::Result<()> {
        let mut line = argus_json::write(&frame.to_json());
        line.push('\n');
        self.writer.write_all(line.as_bytes())
    }

    fn id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// The next frame, or `None` once the host has closed the connection.
    fn next(&mut self) -> Result<Option<ServerFrame>, ApiError> {
        let mut line = String::new();
        if self.reader.read_line(&mut line).map_err(lost)? == 0 {
            return Ok(None);
        }
        let json =
            argus_json::parse(line.trim_end().as_bytes()).map_err(|e| lost(format!("{e:?}")))?;
        let frame =
            ServerFrame::from_json(&json).map_err(|e| lost(format!("{}: {}", e.path, e.what)))?;
        Ok(Some(frame))
    }

    /// Hands each value of the subscription to `on_value` until the host closes the
    /// connection. A subscription the host refuses is answered by a `Reply` with the error.
    pub(crate) fn subscribe(
        &mut self,
        op: &str,
        input: Json,
        mut on_value: impl FnMut(Json),
    ) -> Result<(), ApiError> {
        let id = self.id();
        self.send(&ClientFrame::Subscribe {
            id,
            op: op.into(),
            input,
        })
        .map_err(lost)?;
        while let Some(frame) = self.next()? {
            match frame {
                ServerFrame::Push { id: pushed, value } if pushed == id => on_value(value),
                ServerFrame::Reply {
                    id: replied,
                    err: Some(err),
                    ..
                } if replied == id => return Err(err),
                _ => {}
            }
        }
        Ok(())
    }
}

impl Transport for SocketTransport {
    fn call(&mut self, op: &str, input: Json) -> Result<Json, ApiError> {
        let id = self.id();
        self.send(&ClientFrame::Call {
            id,
            op: op.into(),
            input,
        })
        .map_err(lost)?;
        while let Some(frame) = self.next()? {
            if let ServerFrame::Reply {
                id: replied,
                ok,
                err,
            } = frame
                && replied == id
            {
                return match (ok, err) {
                    (_, Some(err)) => Err(err),
                    (Some(ok), None) => Ok(ok),
                    (None, None) => Err(lost("a reply with neither a value nor an error")),
                };
            }
        }
        // The host answers every request it read, so this is a `Hello` it refused.
        Err(lost(
            "the host closed the connection before it replied; it does so when \
             ARGUS_RUN and ARGUS_TOKEN do not name a run that is running now",
        ))
    }
}
