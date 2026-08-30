use super::*;

pub(super) fn spawn_frame_reader(
    mut reader: OwnedReadHalf,
    version: RawProtocolVersion,
    resources: LocalRawSocketResources,
    resource_read_state: ResourceReadState,
    sender: mpsc::Sender<Result<Option<IncomingFrame>, LocalRawSocketError>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let frame = read_versioned_client_frame_with_resource_state(
                &mut reader,
                &version,
                &resources,
                Instant::now() + FRAME_IDLE_TIMEOUT,
                &resource_read_state,
            )
            .await;
            let terminal = !matches!(frame, Ok(Some(_)));
            if sender.send(frame).await.is_err() || terminal {
                return;
            }
        }
    })
}

#[derive(Clone, Copy)]
enum FrameReadMode<'a> {
    #[cfg(test)]
    Fixed(Instant),
    ResourceAware {
        deadline: Instant,
        state: &'a ResourceReadState,
    },
}

impl FrameReadMode<'_> {
    fn deadline(self) -> Instant {
        match self {
            #[cfg(test)]
            Self::Fixed(deadline) => deadline,
            Self::ResourceAware { deadline, .. } => deadline,
        }
    }

    fn resource_active(self) -> bool {
        match self {
            #[cfg(test)]
            Self::Fixed(_) => false,
            Self::ResourceAware { state, .. } => state.is_active(),
        }
    }
}

pub(super) fn resource_idle_timeout_is_retryable(
    resource_active: bool,
    header_bytes: usize,
    deadline: Instant,
    now: Instant,
) -> bool {
    resource_active && header_bytes == 0 && now >= deadline
}

#[cfg(test)]
pub(super) async fn read_client_frame<R: AsyncRead + Unpin>(
    stream: &mut R,
    resources: &LocalRawSocketResources,
    deadline: Instant,
) -> Result<Option<IncomingFrame>, LocalRawSocketError> {
    read_versioned_client_frame(stream, &RawProtocolVersion::One, resources, deadline).await
}

#[cfg(test)]
async fn read_versioned_client_frame<R: AsyncRead + Unpin>(
    stream: &mut R,
    version: &RawProtocolVersion,
    resources: &LocalRawSocketResources,
    deadline: Instant,
) -> Result<Option<IncomingFrame>, LocalRawSocketError> {
    read_versioned_client_frame_with_mode(
        stream,
        version,
        resources,
        FrameReadMode::Fixed(deadline),
    )
    .await
}

async fn read_versioned_client_frame_with_resource_state<R: AsyncRead + Unpin>(
    stream: &mut R,
    version: &RawProtocolVersion,
    resources: &LocalRawSocketResources,
    deadline: Instant,
    state: &ResourceReadState,
) -> Result<Option<IncomingFrame>, LocalRawSocketError> {
    read_versioned_client_frame_with_mode(
        stream,
        version,
        resources,
        FrameReadMode::ResourceAware { deadline, state },
    )
    .await
}

async fn read_versioned_client_frame_with_mode<R: AsyncRead + Unpin>(
    stream: &mut R,
    version: &RawProtocolVersion,
    resources: &LocalRawSocketResources,
    mode: FrameReadMode<'_>,
) -> Result<Option<IncomingFrame>, LocalRawSocketError> {
    let mut header = [0_u8; SESSION_HEADER_LENGTH];
    let Some(frame_deadline) =
        read_header_before(stream, &mut header[..SESSION_MARKER.len()], mode).await?
    else {
        return Ok(None);
    };
    let session = &header[..SESSION_MARKER.len()] == SESSION_MARKER;
    if !session {
        read_exact_before(
            stream,
            &mut header[SESSION_MARKER.len()..RESOURCE_MARKER.len()],
            frame_deadline,
            LocalRawSocketError::FrameTimeout,
        )
        .await?;
    }
    let resource = !session && &header[..RESOURCE_MARKER.len()] == RESOURCE_MARKER;
    let header_length = if session {
        SESSION_HEADER_LENGTH
    } else if resource {
        RESOURCE_HEADER_LENGTH
    } else {
        FRAME_HEADER_LENGTH
    };
    let consumed = if session {
        SESSION_MARKER.len()
    } else {
        RESOURCE_MARKER.len()
    };
    read_exact_before(
        stream,
        &mut header[consumed..header_length],
        frame_deadline,
        LocalRawSocketError::FrameTimeout,
    )
    .await?;
    let declared_offset = if session {
        SESSION_HEADER_LENGTH - std::mem::size_of::<u32>()..SESSION_HEADER_LENGTH
    } else if resource {
        17..21
    } else {
        14..18
    };
    let declared = u32::from_be_bytes(
        header[declared_offset]
            .try_into()
            .expect("fixed frame header"),
    ) as usize;
    if session && declared > orna_protocol::MAX_SESSION_FRAME_LENGTH - SESSION_HEADER_LENGTH {
        return Err(LocalRawSocketError::Session {
            source: SessionCodecError::Oversize,
        });
    }
    if declared > MAX_FRAME_PAYLOAD_LENGTH {
        return Err(LocalRawSocketError::Frame {
            source: FrameCodecError::PayloadTooLarge {
                actual: declared,
                maximum: MAX_FRAME_PAYLOAD_LENGTH,
            },
        });
    }
    let reservation = resources.reserve_payload(declared)?;
    let mut encoded = Vec::with_capacity(header_length + declared);
    encoded.extend_from_slice(&header[..header_length]);
    encoded.resize(header_length + declared, 0);
    read_exact_before(
        stream,
        &mut encoded[header_length..],
        frame_deadline,
        LocalRawSocketError::FrameTimeout,
    )
    .await?;
    if session {
        let frame = decode_session_client_frame(&encoded)
            .map_err(|source| LocalRawSocketError::Session { source })?;
        Ok(Some(IncomingFrame::Session { frame, reservation }))
    } else if resource {
        let frame = version
            .decode_resource_client_frame(&encoded)
            .map_err(|source| LocalRawSocketError::Frame { source })?;
        Ok(Some(IncomingFrame::Resource { frame, reservation }))
    } else {
        let frame = version
            .decode_client_frame(&encoded)
            .map_err(|source| LocalRawSocketError::Frame { source })?;
        Ok(Some(IncomingFrame::Raw(RawIncomingFrame {
            frame,
            reservation,
        })))
    }
}

async fn read_header_before<R: AsyncRead + Unpin>(
    stream: &mut R,
    header: &mut [u8],
    mode: FrameReadMode<'_>,
) -> Result<Option<Instant>, LocalRawSocketError> {
    let mut filled = 0;
    let mut deadline = mode.deadline();
    while filled < header.len() {
        let read = match timeout_at(deadline, stream.read(&mut header[filled..])).await {
            Ok(read) => read.map_err(|source| LocalRawSocketError::Io { source })?,
            Err(_)
                if resource_idle_timeout_is_retryable(
                    mode.resource_active(),
                    filled,
                    deadline,
                    Instant::now(),
                ) =>
            {
                // A live resource may be waiting indefinitely for client credit.
                // Once a frame has started, the fresh deadline below bounds the
                // remainder and prevents an incomplete frame from pinning a task.
                deadline = Instant::now() + FRAME_IDLE_TIMEOUT;
                continue;
            }
            Err(_) => return Err(LocalRawSocketError::FrameTimeout),
        };
        if read == 0 {
            if filled == 0 {
                return Ok(None);
            }
            return Err(LocalRawSocketError::Io {
                source: io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "raw frame header is truncated",
                ),
            });
        }
        if filled == 0 && mode.resource_active() {
            deadline = Instant::now() + FRAME_IDLE_TIMEOUT;
        }
        filled += read;
    }
    Ok(Some(deadline))
}

pub(super) async fn read_exact_before<R: AsyncRead + Unpin>(
    stream: &mut R,
    bytes: &mut [u8],
    deadline: Instant,
    timeout_error: LocalRawSocketError,
) -> Result<(), LocalRawSocketError> {
    timeout_at(deadline, stream.read_exact(bytes))
        .await
        .map_err(|_| timeout_error)?
        .map_err(|source| LocalRawSocketError::Io { source })?;
    Ok(())
}

pub(super) async fn flush_session_pending<D: DispatchService>(
    dispatcher: &D,
    stream: &mut OwnedWriteHalf,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<bool, LocalRawSocketError> {
    let Some(bridge) = dispatcher.session_bridge() else {
        return Ok(true);
    };
    while let Some(frame) = bridge.try_take_outbound() {
        let encoded = encode_session_server_frame(&frame)
            .map_err(|source| LocalRawSocketError::Session { source })?;
        if !write_all_until_shutdown(stream, &encoded, shutdown).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) async fn wait_for_session_outbound(bridge: Option<Arc<crate::invoke::SessionBridge>>) {
    match bridge {
        Some(bridge) => bridge.wait_for_outbound().await,
        None => std::future::pending::<()>().await,
    }
}

pub(super) async fn write_server_frame(
    version: &RawProtocolVersion,
    stream: &mut OwnedWriteHalf,
    frame: &ServerFrame,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<bool, LocalRawSocketError> {
    let encoded = version
        .encode_server_frame(frame)
        .map_err(|source| LocalRawSocketError::Frame { source })?;
    write_all_until_shutdown(stream, &encoded, shutdown).await
}

pub(super) async fn write_all_until_shutdown<W: tokio::io::AsyncWrite + Unpin>(
    stream: &mut W,
    bytes: &[u8],
    shutdown: &mut watch::Receiver<bool>,
) -> Result<bool, LocalRawSocketError> {
    if *shutdown.borrow() {
        return Ok(false);
    }
    tokio::select! {
        result = stream.write_all(bytes) => {
            result.map_err(|source| LocalRawSocketError::Io { source })?;
            Ok(true)
        }
        _ = wait_for_shutdown(shutdown) => Ok(false),
    }
}

pub(super) fn report_private_dispatch_source(source: &orna_postgres::PostgresKernelError) {
    let _ = writeln!(
        io::stderr().lock(),
        "orna: protected raw client dispatch failed: {source}"
    );
}

pub(super) const fn client_stream(frame: &ClientFrame) -> u64 {
    match frame {
        ClientFrame::CallRawStart { stream, .. }
        | ClientFrame::CallArgument { stream, .. }
        | ClientFrame::CallInvokeRequest { stream, .. }
        | ClientFrame::CallArgumentsComplete { stream }
        | ClientFrame::WindowUpdate { stream, .. }
        | ClientFrame::CallCancel { stream } => *stream,
        ClientFrame::Ping { .. } => 0,
    }
}
