use crate::{
    AndroidFrameworkFilesystemDispatcher, AndroidShizukuFilesystemPort,
    dispatch_android_execution_for, dispatch_android_execution_for_with_descriptor,
};
use contract::{
    ErrorCode, FileTarget, FileTargetType, FilesystemCall, FilesystemInspectInput, ImageFormat,
    Region,
};
use domain::{DomainError, ExecutorRequest, Preflight, VisualRoute};
use runtime::{
    AdmittedExecution, AndroidFrameworkFilesystemPort, CapabilityPort, CommandProcessCause,
    ExecutionFailure, ExecutorRecord, FilesystemCandidate, FilesystemFrameworkPort,
    FilesystemPreflightPort, FilesystemPrimitivePort, LocalExecutionClaim, ProviderToken,
    VisualDisplaySnapshot, VisualEncodedImage, VisualHierarchySnapshot, VisualInteractionRequest,
    VisualPrimitivePort, VisualTransformSource, decode_android_command_settlement,
    parse_privileged_hierarchy, resolve_filesystem_executor, shizuku_filesystem_preflight,
};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub(crate) struct ApkVisualPort<C> {
    canonical_base: PathBuf,
    capabilities: C,
    framework: AndroidFrameworkFilesystemPort<AndroidFrameworkFilesystemDispatcher>,
    shizuku: AndroidShizukuFilesystemPort,
}

impl<C> ApkVisualPort<C> {
    pub(crate) fn new(canonical_base: PathBuf, capabilities: C) -> Self {
        cleanup_visual_temps(&canonical_base);
        Self {
            canonical_base,
            capabilities,
            framework: AndroidFrameworkFilesystemPort::new(AndroidFrameworkFilesystemDispatcher),
            shizuku: AndroidShizukuFilesystemPort,
        }
    }
}

impl<C> VisualPrimitivePort for ApkVisualPort<C>
where
    C: CapabilityPort + Clone,
{
    fn display(
        &self,
        execution: &AdmittedExecution,
        claim: &LocalExecutionClaim,
    ) -> Result<VisualDisplaySnapshot, ExecutionFailure> {
        claim.checkpoint().map_err(clean_failure)?;
        if execution.executor.provider != ProviderToken::AppFramework {
            return Err(stale("display primitive is not bound to the App framework"));
        }
        let result = dispatch_no_descriptors(
            "android.framework",
            "VisualDisplaySnapshot",
            &serde_json::json!({"operation":"display"}),
            execution,
        )
        .map_err(clean_failure)?;
        runtime::decode_display(&result.payload).map_err(clean_failure)
    }

    fn capture_image(
        &self,
        execution: &AdmittedExecution,
        display: &VisualDisplaySnapshot,
        claim: &LocalExecutionClaim,
    ) -> Result<VisualEncodedImage, ExecutionFailure> {
        claim.checkpoint().map_err(clean_failure)?;
        match execution.executor.provider {
            ProviderToken::Shizuku => self.capture_shizuku(execution, display, claim),
            ProviderToken::Accessibility => dispatch_encoded_image(
                "visual.accessibility",
                "AccessibilityObserve",
                &serde_json::json!({
                    "operation":"screenshot",
                    "display":display.display,
                    "display_generation":display.display_generation,
                }),
                execution,
                None,
                true,
            )
            .map_err(clean_failure),
            ProviderToken::MediaProjection => dispatch_encoded_image(
                "visual.media_projection_session",
                "MediaProjectionCapture",
                &serde_json::json!({
                    "display":display.display,
                    "display_generation":display.display_generation,
                }),
                execution,
                None,
                true,
            )
            .map_err(clean_failure),
            _ => Err(stale("image primitive provider is invalid")),
        }
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
        match execution.executor.provider {
            ProviderToken::Shizuku => {
                let xml = self.dump_shizuku_hierarchy(execution, claim)?;
                parse_privileged_hierarchy(&xml, max_nodes, display.clone()).map_err(clean_failure)
            }
            ProviderToken::Accessibility => {
                let result = dispatch_no_descriptors(
                    "visual.accessibility",
                    "AccessibilityObserve",
                    &serde_json::json!({
                        "operation":"hierarchy",
                        "observation_id":observation_id,
                        "max_nodes":max_nodes,
                        "display":display.display,
                        "display_generation":display.display_generation,
                    }),
                    execution,
                )
                .map_err(clean_failure)?;
                runtime::decode_accessibility_hierarchy(&result.payload).map_err(clean_failure)
            }
            _ => Err(stale("hierarchy primitive provider is invalid")),
        }
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
            return Err(stale("visual transform is not bound to the App framework"));
        }
        let source_file = self
            .open_transform_source(execution, source, claim)
            .map_err(clean_failure)?;
        dispatch_encoded_image(
            "android.framework",
            "VisualImageTransform",
            &serde_json::json!({"region":region}),
            execution,
            Some(("visual_source_image", &source_file)),
            false,
        )
        .map_err(clean_failure)
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
                let proof = runtime::accessibility_proof(&proof)
                    .ok_or_else(|| stale("Accessibility interaction proof is invalid"))?;
                dispatch_completed(
                    "visual.accessibility",
                    if operation == "text" {
                        "AccessibilityText"
                    } else {
                        "AccessibilityNodeAction"
                    },
                    &serde_json::json!({
                        "observation_id":observation_id,
                        "node_ref":node_ref,
                        "operation":operation,
                        "text":text,
                        "display":display.display,
                        "display_generation":display.display_generation,
                        "proof":proof,
                    }),
                    execution,
                )
                .map_err(clean_failure)
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
                dispatch_completed(
                    "visual.accessibility",
                    "AccessibilityGesture",
                    &serde_json::json!({
                        "observation_id":observation_id,
                        "operation":operation,
                        "from_x":from_x,"from_y":from_y,"to_x":to_x,"to_y":to_y,
                        "duration_ms":duration_ms,
                        "display":display.display,
                        "display_generation":display.display_generation,
                        "proof":proof,
                    }),
                    execution,
                )
                .map_err(clean_failure)
            }
            (ProviderToken::Accessibility, VisualInteractionRequest::FocusedText { text }) => {
                dispatch_completed(
                    "visual.accessibility",
                    "AccessibilityText",
                    &serde_json::json!({"operation":"focused","text":text}),
                    execution,
                )
                .map_err(clean_failure)
            }
            (ProviderToken::Shizuku, request) => self.interact_shizuku(execution, request, claim),
            _ => Err(stale("visual interaction provider is invalid")),
        }
    }
}

impl<C> ApkVisualPort<C>
where
    C: CapabilityPort + Clone,
{
    fn capture_shizuku(
        &self,
        execution: &AdmittedExecution,
        display: &VisualDisplaySnapshot,
        claim: &LocalExecutionClaim,
    ) -> Result<VisualEncodedImage, ExecutionFailure> {
        self.capture_shizuku_png(execution, display, claim)
    }

    fn capture_shizuku_png(
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
            self.run_shizuku_output(
                execution,
                claim,
                "screen_capture",
                &serde_json::json!({}),
                temporary.writer(),
            )?;
            let bytes = temporary
                .read_bounded(8 * 1_024 * 1_024)
                .map_err(clean_failure)?;
            let (width, height) = runtime::png_dimensions(&bytes).map_err(clean_failure)?;
            if width != display.display.width || height != display.display.height {
                return Err(clean_failure(DomainError::new(
                    ErrorCode::StaleAuthority,
                    "PNG screen capture display changed",
                )));
            }
            let reader = temporary.read_only().map_err(clean_failure)?;
            let encoded = dispatch_encoded_image(
                "android.framework",
                "VisualImageTransform",
                &serde_json::json!({"region":null}),
                &self.codec_execution(execution).map_err(clean_failure)?,
                Some(("visual_source_image", &reader)),
                false,
            )
            .map_err(clean_failure)?;
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

    fn dump_shizuku_hierarchy(
        &self,
        execution: &AdmittedExecution,
        claim: &LocalExecutionClaim,
    ) -> Result<Vec<u8>, ExecutionFailure> {
        let path = PathBuf::from(format!(
            "/data/local/tmp/droidbridge-ui-{}.xml",
            execution.execution_id.as_str()
        ));
        let run = self.run_shizuku(
            execution,
            claim,
            "uiautomator_dump",
            &serde_json::json!({"request_temp_path":path}),
            None,
        );
        if let Err(error) = run {
            return match self.cleanup_shizuku_temp(execution, &path) {
                Ok(()) => Err(error),
                Err(_) => Err(ExecutionFailure {
                    error: DomainError::new(
                        ErrorCode::IoError,
                        "privileged hierarchy cleanup failed",
                    ),
                    cleanup_verified: false,
                }),
            };
        }
        let read = self
            .shizuku
            .open_read(execution, &path)
            .map_err(clean_failure)
            .and_then(|mut file| read_bounded(&mut file, 8 * 1_024 * 1_024).map_err(clean_failure));
        let cleanup = self.cleanup_shizuku_temp(execution, &path);
        match (read, cleanup) {
            (Ok(bytes), Ok(())) => Ok(bytes),
            (Err(error), Ok(())) => Err(error),
            (_, Err(_)) => Err(ExecutionFailure {
                error: DomainError::new(ErrorCode::IoError, "privileged hierarchy cleanup failed"),
                cleanup_verified: false,
            }),
        }
    }

    fn cleanup_shizuku_temp(
        &self,
        execution: &AdmittedExecution,
        path: &Path,
    ) -> Result<(), DomainError> {
        match self.shizuku.unlink(execution, path) {
            Ok(()) => Ok(()),
            Err(error) if error.code == ErrorCode::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn interact_shizuku(
        &self,
        execution: &AdmittedExecution,
        request: VisualInteractionRequest,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        let (operation, payload) = match request {
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
                let current_display = self.display(
                    &self.codec_execution(execution).map_err(clean_failure)?,
                    claim,
                )?;
                if current_display != display {
                    return Err(clean_failure(DomainError::new(
                        ErrorCode::StaleReference,
                        "privileged visual display changed",
                    )));
                }
                let current_scene = parse_privileged_hierarchy(
                    &self.dump_shizuku_hierarchy(execution, claim)?,
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
                    "tap" => ("input_tap", serde_json::json!({"x":from_x,"y":from_y})),
                    "long_press" => (
                        "input_long_press",
                        serde_json::json!({"x":from_x,"y":from_y}),
                    ),
                    "swipe" => (
                        "input_swipe",
                        serde_json::json!({
                            "x1":from_x,"y1":from_y,
                            "x2":to_x,"y2":to_y,
                            "duration_ms":duration_ms,
                        }),
                    ),
                    _ => {
                        return Err(clean_failure(DomainError::new(
                            ErrorCode::Unsupported,
                            "visual coordinate operation is unsupported",
                        )));
                    }
                }
            }
            VisualInteractionRequest::FocusedText { text } => {
                if text.len() > 8_192 {
                    return Err(clean_failure(DomainError::new(
                        ErrorCode::Unsupported,
                        "Shizuku focused text exceeds the input command bound",
                    )));
                }
                // `input text` fails on anything its key character map cannot type (every CJK
                // character, emoji and full-width form). Refuse it before the call instead of
                // letting the framework throw and surface as a misleading I/O failure.
                if !runtime::input_text_delivers(&text) {
                    return Err(clean_failure(DomainError::new(
                        ErrorCode::Unsupported,
                        "Shizuku text input covers printable ASCII only",
                    )));
                }
                ("input_text", serde_json::json!({"text":text}))
            }
            VisualInteractionRequest::Key {
                key_code,
                meta_state,
            } => match runtime::meta_modifier_keys(meta_state).filter(|_| key_code >= 0) {
                Some(keys) if keys.is_empty() => {
                    ("input_key", serde_json::json!({"key_code":key_code}))
                }
                Some(mut keys) => {
                    keys.push(key_code);
                    (
                        "input_key_combination",
                        serde_json::json!({"key_codes":keys}),
                    )
                }
                None => {
                    return Err(clean_failure(DomainError::new(
                        ErrorCode::Unsupported,
                        "Shizuku key input cannot represent the requested meta state",
                    )));
                }
            },
            VisualInteractionRequest::Node { .. } => {
                return Err(stale("privileged XML does not own actionable nodes"));
            }
        };
        self.run_shizuku(execution, claim, operation, &payload, None)
            .map(|_| ())
    }

    fn run_shizuku_output(
        &self,
        execution: &AdmittedExecution,
        claim: &LocalExecutionClaim,
        operation: &str,
        payload: &serde_json::Value,
        output: &File,
    ) -> Result<(), ExecutionFailure> {
        self.run_shizuku(
            execution,
            claim,
            operation,
            payload,
            Some(("visual_output", output)),
        )
        .map(|_| ())
    }

    fn run_shizuku(
        &self,
        execution: &AdmittedExecution,
        claim: &LocalExecutionClaim,
        operation: &str,
        payload: &serde_json::Value,
        descriptor: Option<(&str, &File)>,
    ) -> Result<runtime::CommandProcessSettlement, ExecutionFailure> {
        let mut object = payload.as_object().cloned().ok_or_else(|| {
            clean_failure(DomainError::new(
                ErrorCode::InternalError,
                "visual primitive payload is not an object",
            ))
        })?;
        object.insert(
            "operation".to_owned(),
            serde_json::Value::String(operation.to_owned()),
        );
        let payload = serde_json::to_vec(&object).map_err(|_| {
            clean_failure(DomainError::new(
                ErrorCode::InternalError,
                "visual primitive encoding failed",
            ))
        })?;
        let child = primitive_execution(execution).map_err(clean_failure)?;
        let descriptor = descriptor.map(|(role, file)| (role, raw_fd(file)));
        let result = crate::command::run_shizuku_guarded(&child, claim, &payload, descriptor)
            .map_err(clean_failure)?;
        if !result.descriptors.is_empty() {
            return Err(clean_failure(DomainError::new(
                ErrorCode::IoError,
                "Shizuku visual primitive returned unexpected descriptors",
            )));
        }
        let settlement = decode_android_command_settlement(&result.payload)?;
        if settlement.outcome.cause != CommandProcessCause::Exited
            || settlement.outcome.exit_code != Some(0)
            || settlement.outcome.stdout_truncated
            || settlement.outcome.stderr_truncated
        {
            return Err(clean_failure(DomainError::new(
                ErrorCode::IoError,
                "Shizuku visual primitive failed",
            )));
        }
        Ok(settlement)
    }

    fn codec_execution(
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
    ) -> Result<File, DomainError> {
        match source {
            VisualTransformSource::ImmutableArtifact(bytes) => {
                let temporary = ExecutionTempFile::create(
                    &self.canonical_base,
                    &execution.execution_id,
                    "visual-source.image",
                )?;
                temporary.writer().write_all(&bytes).map_err(io_error)?;
                temporary.persist_for_read()
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
                .map(|source| source.file),
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
                let capability = self.capabilities.current()?;
                let preflight = VisualPathPreflight {
                    shizuku: &self.shizuku,
                    capability: &capability,
                };
                let executor = resolve_filesystem_executor(&capability, &preflight, &call)?
                    .ok_or_else(|| {
                        DomainError::new(
                            ErrorCode::Unsupported,
                            "visual path source has no filesystem executor",
                        )
                    })?;
                if ExecutorRecord::from(&executor) != admitted_executor {
                    return Err(DomainError::new(
                        ErrorCode::StaleAuthority,
                        "visual path source executor changed after admission",
                    ));
                }
                let source_execution = AdmittedExecution {
                    execution_id: execution.execution_id.clone(),
                    task_id: execution.task_id.clone(),
                    executor: ExecutorRecord::from(&executor),
                    payload: runtime::ExecutionPayload::FilesystemCall(call),
                };
                claim.checkpoint()?;
                match source_execution.executor.provider {
                    ProviderToken::AppNative => open_no_follow(&target.value),
                    ProviderToken::Shizuku => self
                        .shizuku
                        .open_read(&source_execution, Path::new(&target.value)),
                    _ => Err(DomainError::new(
                        ErrorCode::StaleAuthority,
                        "visual path provider is invalid",
                    )),
                }
            }
        }
    }
}

#[cfg(unix)]
fn open_no_follow(path: impl AsRef<Path>) -> Result<File, DomainError> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(io_error)
}

#[cfg(not(unix))]
fn open_no_follow(path: impl AsRef<Path>) -> Result<File, DomainError> {
    File::open(path).map_err(io_error)
}

struct VisualPathPreflight<'a> {
    shizuku: &'a AndroidShizukuFilesystemPort,
    capability: &'a runtime::CapabilitySnapshot,
}

impl FilesystemPreflightPort for VisualPathPreflight<'_> {
    fn preflight(
        &self,
        candidate: FilesystemCandidate,
        call: &FilesystemCall,
    ) -> Result<Preflight, DomainError> {
        match candidate {
            FilesystemCandidate::App => runtime::filesystem_preflight(call),
            FilesystemCandidate::Shizuku => {
                shizuku_filesystem_preflight(self.shizuku, self.capability, call)
            }
        }
    }
}

fn dispatch_encoded_image(
    key: &str,
    primitive: &str,
    payload: &serde_json::Value,
    execution: &AdmittedExecution,
    descriptor: Option<(&str, &File)>,
    captured: bool,
) -> Result<VisualEncodedImage, DomainError> {
    let payload = serde_json::to_vec(payload).map_err(|_| {
        DomainError::new(
            ErrorCode::InternalError,
            "visual image request encoding failed",
        )
    })?;
    let mut result = match descriptor {
        Some((role, file)) => dispatch_android_execution_for_with_descriptor(
            key,
            primitive,
            &payload,
            execution,
            Some((role, raw_fd(file))),
        ),
        None => dispatch_android_execution_for(key, primitive, &payload, execution),
    }?;
    if result.descriptors.len() != 1 || result.descriptors[0].0 != "visual_encoded_image" {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "visual image result descriptor set is invalid",
        ));
    }
    let wire = runtime::decode_encoded_image(&result.payload)?;
    let mut file = result.descriptors.remove(0).1;
    let bytes = read_bounded(&mut file, 8 * 1_024 * 1_024)?;
    if bytes.len() as u64 != wire.size {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "visual image size is invalid",
        ));
    }
    runtime::validate_encoded_bytes(wire.format, &bytes)?;
    if wire.format == ImageFormat::Png
        && runtime::png_dimensions(&bytes)? != (wire.width, wire.height)
    {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "visual PNG dimensions do not match metadata",
        ));
    }
    let captured_display = if captured {
        Some(VisualDisplaySnapshot {
            display: wire.display.ok_or_else(|| {
                DomainError::new(ErrorCode::IoError, "captured display is missing")
            })?,
            display_generation: wire.display_generation.ok_or_else(|| {
                DomainError::new(ErrorCode::IoError, "captured display generation is missing")
            })?,
        })
    } else {
        if wire.display.is_some() || wire.display_generation.is_some() {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "transform returned a captured display",
            ));
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

#[cfg(target_os = "android")]
fn raw_fd(file: &File) -> i32 {
    use std::os::fd::AsRawFd;
    file.as_raw_fd()
}

#[cfg(not(target_os = "android"))]
fn raw_fd(_file: &File) -> i32 {
    -1
}

fn dispatch_no_descriptors(
    key: &str,
    primitive: &str,
    payload: &serde_json::Value,
    execution: &AdmittedExecution,
) -> Result<runtime::AndroidPrimitiveResult, DomainError> {
    let payload = serde_json::to_vec(payload).map_err(|_| {
        DomainError::new(
            ErrorCode::InternalError,
            "Android visual request encoding failed",
        )
    })?;
    let result = dispatch_android_execution_for(key, primitive, &payload, execution)?;
    if !result.descriptors.is_empty() {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "Android visual result returned unexpected descriptors",
        ));
    }
    Ok(result)
}

fn dispatch_completed(
    key: &str,
    primitive: &str,
    payload: &serde_json::Value,
    execution: &AdmittedExecution,
) -> Result<(), DomainError> {
    let result = dispatch_no_descriptors(key, primitive, payload, execution)?;
    if result.payload != br#"{"delivered":true}"# {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "Android visual delivery result is invalid",
        ));
    }
    Ok(())
}

fn primitive_execution(execution: &AdmittedExecution) -> Result<AdmittedExecution, DomainError> {
    Ok(AdmittedExecution {
        execution_id: contract::UuidV4::parse(uuid::Uuid::new_v4().to_string())
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "UUID generation failed"))?,
        task_id: execution.task_id.clone(),
        executor: execution.executor.clone(),
        payload: execution.payload.clone(),
    })
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
        let directory = base.join("tmp").join(execution_id.as_str());
        fs::create_dir_all(&directory).map_err(io_error)?;
        let path = directory.join(name);
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(io_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(io_error)?;
        }
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
        writer.flush().map_err(io_error)?;
        writer.sync_all().map_err(io_error)?;
        writer.seek(SeekFrom::Start(0)).map_err(io_error)?;
        read_bounded(&mut writer, limit)
    }

    fn read_only(&self) -> Result<File, DomainError> {
        File::open(&self.path).map_err(io_error)
    }

    fn persist_for_read(mut self) -> Result<File, DomainError> {
        let mut writer = self.writer.take().ok_or_else(|| {
            DomainError::new(ErrorCode::InternalError, "visual source writer is closed")
        })?;
        writer.flush().map_err(io_error)?;
        writer.sync_all().map_err(io_error)?;
        drop(writer);
        let reader = File::open(&self.path).map_err(io_error)?;
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

fn clean_failure(error: DomainError) -> ExecutionFailure {
    ExecutionFailure {
        error,
        cleanup_verified: true,
    }
}

fn stale(message: &'static str) -> ExecutionFailure {
    clean_failure(DomainError::new(ErrorCode::StaleAuthority, message))
}

fn io_error(_error: std::io::Error) -> DomainError {
    DomainError::new(ErrorCode::IoError, "visual file operation failed")
}
