use crate::{
    android::HelperPort,
    command::{RootCommandGuard, VisualRootPrimitive},
    companion::{CompanionPort, CompanionPrimitiveRequest, CompanionPrimitiveResult},
};
use contract::{
    ErrorCode, FileTarget, FileTargetType, FilesystemCall, FilesystemInspectInput, ImageFormat,
    Region,
};
use domain::{DomainError, ExecutorRequest, Preflight, VisualRoute};
use runtime::{
    AdmittedExecution, AndroidFrameworkFilesystemPort, CapabilityPort, ExecutionFailure,
    ExecutorRecord, FilesystemCandidate, FilesystemFrameworkPort, FilesystemPreflightPort,
    LocalExecutionClaim, ProviderToken, VisualDisplaySnapshot, VisualEncodedImage,
    VisualHierarchySnapshot, VisualInteractionRequest, VisualPrimitivePort, VisualTransformSource,
    input_text_delivers, meta_modifier_keys, parse_privileged_hierarchy,
    resolve_filesystem_executor,
};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::OwnedFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, chown},
    },
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

const COMPANION_POLL: Duration = Duration::from_millis(25);
const COMPANION_DEADLINE: Duration = Duration::from_millis(15_000);
const PNG_LIMIT: usize = 8 * 1_024 * 1_024;

#[derive(Clone)]
pub(crate) struct MagiskVisualPort<C> {
    canonical_base: PathBuf,
    capabilities: C,
    root: Arc<RootCommandGuard>,
    companion: CompanionPort,
    framework: AndroidFrameworkFilesystemPort<CompanionPort>,
    helper: HelperPort,
}

/// `KEYCODE_PASTE`: the focused editor inserts the primary clip.
const KEYCODE_PASTE: i32 = 279;

/// One paste at a time: two tasks sharing the clipboard would paste each other's text.
static CLIPBOARD_PASTE: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl<C> MagiskVisualPort<C> {
    pub(crate) fn new(
        canonical_base: PathBuf,
        capabilities: C,
        root: Arc<RootCommandGuard>,
        companion: CompanionPort,
        helper: HelperPort,
    ) -> Self {
        cleanup_visual_temps(&canonical_base);
        Self {
            canonical_base,
            capabilities,
            helper,
            root,
            framework: AndroidFrameworkFilesystemPort::new(companion.clone()),
            companion,
        }
    }
}

impl<C> VisualPrimitivePort for MagiskVisualPort<C>
where
    C: CapabilityPort + Clone,
{
    fn display(
        &self,
        execution: &AdmittedExecution,
        claim: &LocalExecutionClaim,
    ) -> Result<VisualDisplaySnapshot, ExecutionFailure> {
        claim.checkpoint().map_err(clean_failure)?;
        if execution.executor.provider != ProviderToken::MagiskNative {
            return Err(stale("Magisk display admission has a different provider"));
        }
        let framework = self.framework_execution(execution).map_err(clean_failure)?;
        let result = self.companion_call(
            &framework,
            "VisualDisplaySnapshot",
            serde_json::json!({"operation":"display"}),
            Vec::new(),
            claim,
        )?;
        require_no_descriptors(&result)?;
        runtime::decode_display_value(result.payload).map_err(clean_failure)
    }

    fn capture_image(
        &self,
        execution: &AdmittedExecution,
        display: &VisualDisplaySnapshot,
        claim: &LocalExecutionClaim,
    ) -> Result<VisualEncodedImage, ExecutionFailure> {
        claim.checkpoint().map_err(clean_failure)?;
        if execution.executor.provider != ProviderToken::MagiskNative {
            return Err(stale("Magisk image admission has a different provider"));
        }
        self.capture_root(execution, display, claim)
    }

    fn observe_hierarchy(
        &self,
        execution: &AdmittedExecution,
        display: &VisualDisplaySnapshot,
        observation_id: &contract::UuidV4,
        max_nodes: u32,
        claim: &LocalExecutionClaim,
    ) -> Result<VisualHierarchySnapshot, ExecutionFailure> {
        claim.checkpoint().map_err(clean_failure)?;
        if execution.executor.provider == ProviderToken::Accessibility {
            let result = self.companion_call(
                execution,
                "AccessibilityObserve",
                serde_json::json!({
                    "operation":"hierarchy",
                    "observation_id":observation_id,
                    "max_nodes":max_nodes,
                    "display":display.display,
                    "display_generation":display.display_generation,
                }),
                Vec::new(),
                claim,
            )?;
            require_no_descriptors(&result)?;
            return runtime::decode_accessibility_hierarchy_value(result.payload)
                .map_err(clean_failure);
        }
        if execution.executor.provider != ProviderToken::MagiskNative {
            return Err(stale("Magisk hierarchy admission has a different provider"));
        }
        let xml = self.dump_hierarchy(execution, claim)?;
        parse_privileged_hierarchy(&xml, max_nodes, display.clone()).map_err(clean_failure)
    }

    fn transform(
        &self,
        execution: &AdmittedExecution,
        source: VisualTransformSource,
        region: Option<Region>,
        claim: &LocalExecutionClaim,
    ) -> Result<VisualEncodedImage, ExecutionFailure> {
        claim.checkpoint().map_err(clean_failure)?;
        if execution.executor.provider != ProviderToken::AppFramework {
            return Err(stale(
                "Magisk visual transform lost the App framework provider",
            ));
        }
        let source = self.open_transform_source(execution, source, claim)?;
        self.companion_image(
            execution,
            "VisualImageTransform",
            serde_json::json!({"region":region}),
            Some(("visual_source_image", source)),
            false,
            claim,
        )
    }

    fn interact(
        &self,
        execution: &AdmittedExecution,
        request: VisualInteractionRequest,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        claim.checkpoint().map_err(clean_failure)?;
        match (&execution.executor.provider, request) {
            (
                ProviderToken::Accessibility,
                VisualInteractionRequest::Node {
                    observation_id,
                    node_ref,
                    operation,
                    text,
                    display,
                    proof,
                },
            ) => {
                let primitive = if operation == "text" {
                    "AccessibilityText"
                } else {
                    "AccessibilityNodeAction"
                };
                self.companion_delivered(
                    execution,
                    primitive,
                    serde_json::json!({
                        "observation_id":observation_id,
                        "node_ref":node_ref,
                        "operation":operation,
                        "text":text,
                        "display":display.display,
                        "display_generation":display.display_generation,
                        "proof":runtime::accessibility_proof(&proof)
                    .ok_or_else(|| stale("Accessibility interaction proof is invalid"))?,
                    }),
                    claim,
                )
            }
            (
                ProviderToken::Accessibility,
                VisualInteractionRequest::Coordinate {
                    observation_id,
                    operation,
                    from_x,
                    from_y,
                    to_x,
                    to_y,
                    duration_ms,
                    display,
                    proof,
                },
            ) => {
                let proof = runtime::accessibility_proof(&proof)
                    .ok_or_else(|| stale("Accessibility interaction proof is invalid"))?;
                self.companion_delivered(
                    execution,
                    "AccessibilityGesture",
                    serde_json::json!({
                        "observation_id":observation_id,
                        "operation":operation,
                        "from_x":from_x,"from_y":from_y,"to_x":to_x,"to_y":to_y,
                        "duration_ms":duration_ms,
                        "display":display.display,
                        "display_generation":display.display_generation,
                        "proof":proof,
                    }),
                    claim,
                )
            }
            (ProviderToken::Accessibility, VisualInteractionRequest::FocusedText { text }) => self
                .companion_delivered(
                    execution,
                    "AccessibilityText",
                    serde_json::json!({"operation":"focused","text":text}),
                    claim,
                ),
            (ProviderToken::MagiskNative, request) => self.interact_root(execution, request, claim),
            _ => Err(stale("Magisk visual interaction provider is invalid")),
        }
    }
}

impl<C> MagiskVisualPort<C>
where
    C: CapabilityPort + Clone,
{
    fn capture_root(
        &self,
        execution: &AdmittedExecution,
        display: &VisualDisplaySnapshot,
        claim: &LocalExecutionClaim,
    ) -> Result<VisualEncodedImage, ExecutionFailure> {
        self.capture_png(execution, display, claim)
    }

    fn capture_png(
        &self,
        execution: &AdmittedExecution,
        display: &VisualDisplaySnapshot,
        claim: &LocalExecutionClaim,
    ) -> Result<VisualEncodedImage, ExecutionFailure> {
        let mut temporary = ExecutionTempFile::create(
            &self.canonical_base,
            &execution.execution_id,
            "visual-frame.png",
        )
        .map_err(clean_failure)?;
        let result = (|| {
            self.root.run_visual(
                &execution.execution_id,
                VisualRootPrimitive::ScreenshotPng,
                Some(temporary.writer()),
                claim,
            )?;
            let bytes = temporary.read_bounded(PNG_LIMIT).map_err(clean_failure)?;
            let (width, height) = runtime::png_dimensions(&bytes).map_err(clean_failure)?;
            if width != display.display.width || height != display.display.height {
                return Err(clean_failure(DomainError::new(
                    ErrorCode::StaleAuthority,
                    "PNG screen capture display changed",
                )));
            }
            let framework = self.framework_execution(execution).map_err(clean_failure)?;
            let reader = temporary.read_only().map_err(clean_failure)?;
            let encoded = self.companion_image(
                &framework,
                "VisualImageTransform",
                serde_json::json!({"region":null}),
                Some(("visual_source_image", reader)),
                false,
                claim,
            )?;
            if encoded.width != width || encoded.height != height {
                return Err(clean_failure(DomainError::new(
                    ErrorCode::IoError,
                    "compressed screen capture changed image dimensions",
                )));
            }
            Ok(VisualEncodedImage {
                captured_display: Some(display.clone()),
                ..encoded
            })
        })();
        finish_temp(temporary, result)
    }

    fn dump_hierarchy(
        &self,
        execution: &AdmittedExecution,
        claim: &LocalExecutionClaim,
    ) -> Result<Vec<u8>, ExecutionFailure> {
        let path = PathBuf::from(format!(
            "/data/local/tmp/droidbridge-ui-{}.xml",
            execution.execution_id.as_str()
        ));
        remove_if_present(&path).map_err(clean_failure)?;
        let run = self.root.run_visual(
            &execution.execution_id,
            VisualRootPrimitive::HierarchyDump(path.clone()),
            None,
            claim,
        );
        let read = match run {
            Ok(()) => open_no_follow(&path)
                .and_then(|mut file| read_bounded(&mut file, 8 * 1_024 * 1_024))
                .map_err(clean_failure),
            Err(failure) => Err(failure),
        };
        let cleanup = remove_if_present(&path);
        match (read, cleanup) {
            (Ok(bytes), Ok(())) => Ok(bytes),
            (Err(failure), Ok(())) => Err(failure),
            (_, Err(_)) => Err(ExecutionFailure {
                error: DomainError::new(ErrorCode::IoError, "privileged hierarchy cleanup failed"),
                cleanup_verified: false,
            }),
        }
    }

    fn interact_root(
        &self,
        execution: &AdmittedExecution,
        request: VisualInteractionRequest,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        let primitive = match request {
            VisualInteractionRequest::Coordinate {
                observation_id: _,
                operation,
                from_x,
                from_y,
                to_x,
                to_y,
                duration_ms,
                display,
                proof,
            } => {
                let current_display = self.display(execution, claim)?;
                if current_display != display {
                    return Err(clean_failure(DomainError::new(
                        ErrorCode::StaleReference,
                        "privileged visual display changed",
                    )));
                }
                let current_scene = parse_privileged_hierarchy(
                    &self.dump_hierarchy(execution, claim)?,
                    1,
                    display.clone(),
                )
                .map_err(clean_failure)?;
                if current_scene.proof != proof {
                    return Err(clean_failure(DomainError::new(
                        ErrorCode::StaleReference,
                        "privileged visual scene changed",
                    )));
                }
                match operation.as_str() {
                    "tap" => VisualRootPrimitive::Tap {
                        x: from_x,
                        y: from_y,
                    },
                    "long_press" => VisualRootPrimitive::LongPress {
                        x: from_x,
                        y: from_y,
                    },
                    "swipe" => VisualRootPrimitive::Swipe {
                        from_x,
                        from_y,
                        to_x: to_x.ok_or_else(|| invalid_interaction("swipe x is missing"))?,
                        to_y: to_y.ok_or_else(|| invalid_interaction("swipe y is missing"))?,
                        duration_ms: duration_ms
                            .ok_or_else(|| invalid_interaction("swipe duration is missing"))?,
                    },
                    _ => {
                        return Err(clean_failure(DomainError::new(
                            ErrorCode::Unsupported,
                            "visual coordinate operation is unsupported",
                        )));
                    }
                }
            }
            VisualInteractionRequest::FocusedText { text } => {
                if text.len() > 8_192 || text.contains('\0') {
                    return Err(clean_failure(DomainError::new(
                        ErrorCode::Unsupported,
                        "root focused text exceeds the input command representation",
                    )));
                }
                if !input_text_delivers(&text) {
                    return self.paste_text(execution, text, claim);
                }
                VisualRootPrimitive::Text(text)
            }
            VisualInteractionRequest::Key {
                key_code,
                meta_state,
            } => {
                let modifiers = meta_modifier_keys(meta_state).filter(|_| key_code >= 0);
                match modifiers {
                    Some(keys) if keys.is_empty() => VisualRootPrimitive::Key(key_code),
                    Some(mut keys) => {
                        keys.push(key_code);
                        VisualRootPrimitive::KeyCombination(keys)
                    }
                    None => {
                        return Err(clean_failure(DomainError::new(
                            ErrorCode::Unsupported,
                            "root key input cannot represent the requested meta state",
                        )));
                    }
                }
            }
            VisualInteractionRequest::Node { .. } => {
                return Err(stale("privileged XML does not own actionable nodes"));
            }
        };
        self.root
            .run_visual(&execution.execution_id, primitive, None, claim)
    }

    /// Delivers text `input text` cannot type (anything beyond printable ASCII) by pasting it:
    /// the previous text clip is saved, the text is written marked sensitive so keyboards keep it
    /// out of clipboard previews and history, `KEYCODE_PASTE` is injected and waited for, and
    /// the previous clip is put back (or the clipboard cleared when it held no text).
    fn paste_text(
        &self,
        execution: &AdmittedExecution,
        text: String,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        let _serialized = CLIPBOARD_PASTE.lock().map_err(|_| {
            clean_failure(DomainError::new(
                ErrorCode::InternalError,
                "clipboard paste lock failed",
            ))
        })?;
        let previous = self
            .helper
            .clipboard("read", &serde_json::json!({}), claim)
            .and_then(|value| {
                runtime::decode_clipboard_read(&serde_json::to_vec(&value).map_err(|_| {
                    DomainError::new(ErrorCode::InternalError, "clipboard read encoding failed")
                })?)
            })
            .map_err(clean_failure)?;
        self.helper
            .clipboard(
                "write",
                &serde_json::json!({"text": text, "sensitive": true}),
                claim,
            )
            .map_err(clean_failure)?;
        let pasted = self.root.run_visual(
            &execution.execution_id,
            VisualRootPrimitive::Key(KEYCODE_PASTE),
            None,
            claim,
        );
        let restored = match previous {
            Some(previous) => {
                self.helper
                    .clipboard("write", &serde_json::json!({"text": previous}), claim)
            }
            None => self
                .helper
                .clipboard("clear", &serde_json::json!({}), claim),
        };
        match (pasted, restored) {
            (Err(failure), _) => Err(failure),
            (Ok(()), Ok(_)) => Ok(()),
            // The text reached the editor; the caller must not retry it, but must learn the
            // clipboard still holds it.
            (Ok(()), Err(_)) => Err(clean_failure(DomainError::new(
                ErrorCode::ExecutionFailed,
                "text was pasted but the previous clipboard could not be restored",
            ))),
        }
    }

    fn framework_execution(
        &self,
        execution: &AdmittedExecution,
    ) -> Result<AdmittedExecution, DomainError> {
        let capability = self.capabilities.current()?;
        let executor = domain::resolve_executor(
            capability.context.host,
            capability.fence,
            capability.resolver_facts,
            ExecutorRequest::Visual(VisualRoute::Transform),
        )?;
        Ok(AdmittedExecution {
            execution_id: execution.execution_id.clone(),
            task_id: execution.task_id.clone(),
            executor: ExecutorRecord::from(&executor),
            payload: execution.payload.clone(),
        })
    }

    fn open_transform_source(
        &self,
        execution: &AdmittedExecution,
        source: VisualTransformSource,
        claim: &LocalExecutionClaim,
    ) -> Result<File, ExecutionFailure> {
        let file = match source {
            VisualTransformSource::ImmutableArtifact(bytes) => {
                let temporary = ExecutionTempFile::create(
                    &self.canonical_base,
                    &execution.execution_id,
                    "visual-source.image",
                )
                .map_err(clean_failure)?;
                temporary.writer().write_all(&bytes).map_err(io_failure)?;
                temporary.persist_for_read().map_err(clean_failure)?
            }
            VisualTransformSource::ContentUri(value) => self
                .framework
                .open_read(
                    execution,
                    &FileTarget {
                        target_type: FileTargetType::ContentUri,
                        value,
                    },
                )
                .map(|source| source.file)
                .map_err(clean_failure)?,
            VisualTransformSource::Path {
                path: value,
                executor: admitted_executor,
            } => {
                let target = FileTarget {
                    target_type: FileTargetType::Path,
                    value,
                };
                let call = FilesystemCall::Inspect(FilesystemInspectInput {
                    target: target.clone(),
                    recursive: false,
                    max_depth: 1,
                    max_entries: 200,
                });
                let capability = self.capabilities.current().map_err(clean_failure)?;
                let executor =
                    resolve_filesystem_executor(&capability, &MagiskVisualPathPreflight, &call)
                        .map_err(clean_failure)?
                        .ok_or_else(|| {
                            clean_failure(DomainError::new(
                                ErrorCode::Unsupported,
                                "visual path source has no filesystem executor",
                            ))
                        })?;
                if ExecutorRecord::from(&executor) != admitted_executor {
                    return Err(stale("visual path source executor changed after admission"));
                }
                open_no_follow(Path::new(&target.value)).map_err(clean_failure)?
            }
        };
        claim.checkpoint().map_err(clean_failure)?;
        Ok(file)
    }

    fn companion_delivered(
        &self,
        execution: &AdmittedExecution,
        primitive: &str,
        payload: serde_json::Value,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        let result = self.companion_call(execution, primitive, payload, Vec::new(), claim)?;
        require_no_descriptors(&result)?;
        if result.payload != serde_json::json!({"delivered":true}) {
            return Err(clean_failure(DomainError::new(
                ErrorCode::IoError,
                "Android visual delivery result is invalid",
            )));
        }
        Ok(())
    }

    fn companion_image(
        &self,
        execution: &AdmittedExecution,
        primitive: &str,
        payload: serde_json::Value,
        descriptor: Option<(&str, File)>,
        captured: bool,
        claim: &LocalExecutionClaim,
    ) -> Result<VisualEncodedImage, ExecutionFailure> {
        let descriptors = descriptor
            .map(|(role, file)| vec![(role.to_owned(), OwnedFd::from(file))])
            .unwrap_or_default();
        let mut result = self.companion_call(execution, primitive, payload, descriptors, claim)?;
        if result.descriptors.len() != 1 || result.descriptors[0].0 != "visual_encoded_image" {
            return Err(clean_failure(DomainError::new(
                ErrorCode::IoError,
                "visual image result descriptor set is invalid",
            )));
        }
        let wire = runtime::decode_encoded_image_value(result.payload).map_err(clean_failure)?;
        let (_, descriptor) = result.descriptors.remove(0);
        let mut file = File::from(descriptor);
        let bytes = read_bounded(&mut file, PNG_LIMIT).map_err(clean_failure)?;
        if bytes.len() as u64 != wire.size {
            return Err(clean_failure(DomainError::new(
                ErrorCode::IoError,
                "visual image size is invalid",
            )));
        }
        runtime::validate_encoded_bytes(wire.format, &bytes).map_err(clean_failure)?;
        if wire.format == ImageFormat::Png
            && runtime::png_dimensions(&bytes).map_err(clean_failure)? != (wire.width, wire.height)
        {
            return Err(clean_failure(DomainError::new(
                ErrorCode::IoError,
                "visual PNG dimensions do not match metadata",
            )));
        }
        let captured_display = if captured {
            Some(VisualDisplaySnapshot {
                display: wire.display.ok_or_else(|| {
                    clean_failure(DomainError::new(
                        ErrorCode::IoError,
                        "captured display is missing",
                    ))
                })?,
                display_generation: wire.display_generation.ok_or_else(|| {
                    clean_failure(DomainError::new(
                        ErrorCode::IoError,
                        "captured display generation is missing",
                    ))
                })?,
            })
        } else {
            if wire.display.is_some() || wire.display_generation.is_some() {
                return Err(clean_failure(DomainError::new(
                    ErrorCode::IoError,
                    "transform returned a captured display",
                )));
            }
            None
        };
        Ok(VisualEncodedImage {
            bytes,
            format: wire.format,
            width: wire.width,
            height: wire.height,
            captured_display,
        })
    }

    fn companion_call(
        &self,
        execution: &AdmittedExecution,
        primitive: &str,
        payload: serde_json::Value,
        descriptors: Vec<(String, OwnedFd)>,
        claim: &LocalExecutionClaim,
    ) -> Result<CompanionPrimitiveResult, ExecutionFailure> {
        let (_, mut transaction) = self
            .companion
            .submit(CompanionPrimitiveRequest {
                primitive: primitive.to_owned(),
                payload,
                execution_id: execution.execution_id.clone(),
                descriptors,
            })
            .map_err(clean_failure)?;
        let deadline = Instant::now() + COMPANION_DEADLINE;
        let mut cancelled = false;
        let result = loop {
            cancelled |= claim.checkpoint().is_err();
            match transaction.poll(COMPANION_POLL) {
                Some(result) => break result,
                None if Instant::now() < deadline => {}
                None => {
                    return Err(ExecutionFailure {
                        error: DomainError::new(
                            ErrorCode::Timeout,
                            "companion visual primitive did not settle within its deadline",
                        ),
                        cleanup_verified: false,
                    });
                }
            }
        };
        let result = result.map_err(|error| ExecutionFailure {
            cleanup_verified: error.reason == "companion execution reported a typed failure",
            error,
        })?;
        if cancelled || claim.checkpoint().is_err() {
            return Err(clean_failure(DomainError::new(
                ErrorCode::Cancelled,
                "visual execution was cancelled",
            )));
        }
        Ok(result)
    }
}

struct MagiskVisualPathPreflight;

impl FilesystemPreflightPort for MagiskVisualPathPreflight {
    fn preflight(
        &self,
        _candidate: FilesystemCandidate,
        _call: &FilesystemCall,
    ) -> Result<Preflight, DomainError> {
        Ok(Preflight::Unknown)
    }
}

struct ExecutionTempFile {
    path: PathBuf,
    writer: Option<File>,
}

impl ExecutionTempFile {
    fn create(
        base: &Path,
        execution_id: &contract::UuidV4,
        name: &str,
    ) -> Result<Self, DomainError> {
        let directory = scratch_root(base)?.join(execution_id.as_str());
        fs::create_dir_all(&directory).map_err(io_error)?;
        let path = directory.join(name);
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(io_error)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(io_error)?;
        Ok(Self {
            path,
            writer: Some(file),
        })
    }

    fn writer(&self) -> &File {
        self.writer
            .as_ref()
            .expect("visual temp writer remains open")
    }

    fn read_bounded(&mut self, limit: usize) -> Result<Vec<u8>, DomainError> {
        let mut writer = self.writer.take().ok_or_else(|| {
            DomainError::new(ErrorCode::InternalError, "visual temp writer is closed")
        })?;
        writer.seek(SeekFrom::Start(0)).map_err(io_error)?;
        read_bounded(&mut writer, limit)
    }

    fn read_only(&self) -> Result<File, DomainError> {
        open_no_follow(&self.path)
    }

    fn persist_for_read(mut self) -> Result<File, DomainError> {
        let mut writer = self.writer.take().ok_or_else(|| {
            DomainError::new(ErrorCode::InternalError, "visual source writer is closed")
        })?;
        writer.flush().map_err(io_error)?;
        writer.sync_all().map_err(io_error)?;
        drop(writer);
        let reader = open_no_follow(&self.path)?;
        self.cleanup()?;
        Ok(reader)
    }

    fn cleanup(&mut self) -> Result<(), DomainError> {
        self.writer.take();
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(error)),
        }
        if let Some(parent) = self.path.parent() {
            let _ = fs::remove_dir(parent);
        }
        Ok(())
    }
}

impl Drop for ExecutionTempFile {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn finish_temp<T>(
    mut temporary: ExecutionTempFile,
    result: Result<T, ExecutionFailure>,
) -> Result<T, ExecutionFailure> {
    match (result, temporary.cleanup()) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(failure), Ok(())) => Err(failure),
        (_, Err(error)) => Err(ExecutionFailure {
            error,
            cleanup_verified: false,
        }),
    }
}

fn require_no_descriptors(result: &CompanionPrimitiveResult) -> Result<(), ExecutionFailure> {
    if result.descriptors.is_empty() {
        Ok(())
    } else {
        Err(clean_failure(io_domain(
            "Android visual result returned unexpected descriptors",
        )))
    }
}

fn open_no_follow(path: &Path) -> Result<File, DomainError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(io_error)?;
    if !file.metadata().map_err(io_error)?.is_file() {
        return Err(io_domain("visual source is not a regular file"));
    }
    Ok(file)
}

fn read_bounded(file: &mut File, limit: usize) -> Result<Vec<u8>, DomainError> {
    let mut bytes = Vec::new();
    file.take((limit as u64) + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > limit {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "visual stream exceeds its bound",
        ));
    }
    Ok(bytes)
}

fn remove_if_present(path: &Path) -> Result<(), DomainError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

/// The scratch root every visual execution writes through, owned by whoever created it. Both hosts
/// share it, so a root host that creates it must hand it to the store's owner: the app identity
/// cannot chown a root-owned directory, and every app-hosted transform would then fail EACCES until
/// someone deleted the directory by hand.
fn scratch_root(base: &Path) -> Result<PathBuf, DomainError> {
    let scratch = base.join("tmp");
    fs::create_dir_all(&scratch).map_err(io_error)?;
    let owner = fs::metadata(base).map_err(io_error)?;
    let current = fs::metadata(&scratch).map_err(io_error)?;
    if (current.uid(), current.gid()) == (owner.uid(), owner.gid()) {
        return Ok(scratch);
    }
    chown(&scratch, Some(owner.uid()), Some(owner.gid())).map_err(io_error)?;
    fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
    Ok(scratch)
}

fn cleanup_visual_temps(base: &Path) {
    let Ok(entries) = fs::read_dir(base.join("tmp")) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        for name in [
            "visual-frame.raw",
            "visual-frame.png",
            "visual-source.image",
        ] {
            let _ = fs::remove_file(path.join(name));
        }
        let _ = fs::remove_dir(path);
    }
}

fn invalid_interaction(reason: &'static str) -> ExecutionFailure {
    clean_failure(DomainError::invalid(reason))
}

fn clean_failure(error: DomainError) -> ExecutionFailure {
    ExecutionFailure {
        error,
        cleanup_verified: true,
    }
}

fn io_failure(error: std::io::Error) -> ExecutionFailure {
    clean_failure(io_error(error))
}

fn stale(reason: &'static str) -> ExecutionFailure {
    clean_failure(DomainError::new(ErrorCode::StaleAuthority, reason))
}

fn io_domain(reason: &'static str) -> DomainError {
    DomainError::new(ErrorCode::IoError, reason)
}

fn io_error(_error: std::io::Error) -> DomainError {
    io_domain("visual file operation failed")
}
