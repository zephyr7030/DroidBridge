use crate::{
    AndroidFrameworkFilesystemDispatcher, AndroidShizukuFilesystemPort,
    dispatch_android_execution_for, dispatch_android_execution_for_with_descriptor,
};
use contract::{
    DisplayGeometry, ErrorCode, FileTarget, FileTargetType, FilesystemCall, FilesystemInspectInput,
    ImageFormat, Region, VisualNode,
};
use domain::{DomainError, ExecutorRequest, Preflight, VisualRoute};
use runtime::{
    AdmittedExecution, AndroidFrameworkFilesystemPort, CapabilityPort, CommandProcessCause,
    ExecutionFailure, ExecutorRecord, FilesystemCandidate, FilesystemFrameworkPort,
    FilesystemPreflightPort, FilesystemPrimitivePort, LocalExecutionClaim, ProviderToken,
    VisualDisplaySnapshot, VisualEncodedImage, VisualHierarchySnapshot, VisualInteractionRequest,
    VisualPrimitivePort, VisualSceneProof, VisualTransformSource,
    decode_android_command_settlement, parse_privileged_hierarchy, resolve_filesystem_executor,
    shizuku_filesystem_preflight,
};
use serde::Deserialize;
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
            "AccessibilityObserve",
            &serde_json::json!({"operation":"display"}),
            execution,
        )
        .map_err(clean_failure)?;
        decode_display(&result.payload).map_err(clean_failure)
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
                decode_accessibility_hierarchy(&result.payload).map_err(clean_failure)
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
                let proof = accessibility_proof(proof)?;
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
                    operation,
                    from_x,
                    from_y,
                    to_x,
                    to_y,
                    duration_ms,
                    display,
                },
            ) => dispatch_completed(
                "visual.accessibility",
                "AccessibilityGesture",
                &serde_json::json!({
                    "operation":operation,
                    "from_x":from_x,"from_y":from_y,"to_x":to_x,"to_y":to_y,
                    "duration_ms":duration_ms,
                    "display":display.display,
                    "display_generation":display.display_generation,
                }),
                execution,
            )
            .map_err(clean_failure),
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
        let codec_execution = self.codec_execution(execution).map_err(clean_failure)?;
        let snapshot = dispatch_no_descriptors(
            "android.framework",
            "VisualCodecSnapshot",
            &serde_json::json!({
                "width":display.display.width,
                "height":display.display.height,
                "source":"privileged_raw",
            }),
            &codec_execution,
        )
        .map_err(clean_failure)?;
        let snapshot: CodecSnapshot = serde_json::from_slice(&snapshot.payload).map_err(|_| {
            clean_failure(DomainError::new(
                ErrorCode::IoError,
                "visual codec snapshot is invalid",
            ))
        })?;
        match snapshot.hardware_heic.as_str() {
            "unavailable" if snapshot.codec_generation > 0 && snapshot.reason.is_some() => {
                self.capture_shizuku_png(execution, display, claim)
            }
            "available" if snapshot.codec_generation > 0 && snapshot.reason.is_none() => self
                .capture_shizuku_raw(
                    execution,
                    &codec_execution,
                    display,
                    snapshot.codec_generation,
                    claim,
                ),
            _ => Err(clean_failure(DomainError::new(
                ErrorCode::IoError,
                "visual codec snapshot is invalid",
            ))),
        }
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
            let (width, height) = png_dimensions(&bytes).map_err(clean_failure)?;
            if width != display.display.width || height != display.display.height {
                return Err(clean_failure(DomainError::new(
                    ErrorCode::StaleAuthority,
                    "PNG screen capture display changed",
                )));
            }
            Ok(VisualEncodedImage {
                bytes,
                format: ImageFormat::Png,
                width,
                height,
                captured_display: Some(display.clone()),
            })
        })();
        finish_temp(temporary, result)
    }

    fn capture_shizuku_raw(
        &self,
        execution: &AdmittedExecution,
        codec_execution: &AdmittedExecution,
        display: &VisualDisplaySnapshot,
        codec_generation: u64,
        claim: &LocalExecutionClaim,
    ) -> Result<VisualEncodedImage, ExecutionFailure> {
        let mut temporary = ExecutionTempFile::create(
            &self.canonical_base,
            &execution.execution_id,
            "visual-frame.raw",
        )
        .map_err(clean_failure)?;
        let result = (|| {
            self.run_shizuku_output(
                execution,
                claim,
                "screen_capture_raw",
                &serde_json::json!({}),
                temporary.writer(),
            )?;
            let metadata = temporary.raw_metadata().map_err(clean_failure)?;
            if metadata.width != display.display.width || metadata.height != display.display.height
            {
                return Err(clean_failure(DomainError::new(
                    ErrorCode::StaleAuthority,
                    "raw screen capture display changed",
                )));
            }
            let reader = temporary.read_only().map_err(clean_failure)?;
            let encoded = dispatch_encoded_image(
                "android.framework",
                "VisualFrameEncode",
                &serde_json::json!({
                    "width":metadata.width,
                    "height":metadata.height,
                    "pixel_format":metadata.pixel_format,
                    "colorspace":metadata.colorspace,
                    "requested":"heic",
                    "codec_generation":codec_generation,
                }),
                codec_execution,
                Some(("visual_raw_frame", &reader)),
                false,
            )
            .or_else(|_| {
                dispatch_encoded_image(
                    "android.framework",
                    "VisualFrameEncode",
                    &serde_json::json!({
                        "width":metadata.width,
                        "height":metadata.height,
                        "pixel_format":metadata.pixel_format,
                        "colorspace":metadata.colorspace,
                        "requested":"png",
                    }),
                    codec_execution,
                    Some(("visual_raw_frame", &reader)),
                    false,
                )
            })
            .map_err(clean_failure)?;
            drop(reader);
            if encoded.width != metadata.width || encoded.height != metadata.height {
                return Err(clean_failure(DomainError::new(
                    ErrorCode::IoError,
                    "raw frame encoder changed image dimensions",
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
                operation,
                from_x,
                from_y,
                to_x,
                to_y,
                duration_ms,
                display,
            } => {
                // The display the caller saw is the whole coordinate contract; the hierarchy may have
                // repainted since, and the runtime owns the observation's lifetime.
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodecSnapshot {
    hardware_heic: String,
    codec_generation: u64,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayWire {
    display: DisplayGeometry,
    display_generation: u64,
}

fn decode_display(payload: &[u8]) -> Result<VisualDisplaySnapshot, DomainError> {
    let wire: DisplayWire = serde_json::from_slice(payload)
        .map_err(|_| DomainError::new(ErrorCode::IoError, "visual display result is invalid"))?;
    Ok(VisualDisplaySnapshot {
        display: wire.display,
        display_generation: wire.display_generation,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessibilityHierarchyWire {
    display: DisplayGeometry,
    display_generation: u64,
    #[serde(default)]
    foreground: Option<contract::ForegroundFact>,
    nodes: Vec<VisualNode>,
    truncated: bool,
    component_generation: u64,
    window_id: i32,
    scene_revision: u64,
    hierarchy_sha256: String,
}

fn decode_accessibility_hierarchy(payload: &[u8]) -> Result<VisualHierarchySnapshot, DomainError> {
    let wire: AccessibilityHierarchyWire = serde_json::from_slice(payload).map_err(|_| {
        DomainError::new(
            ErrorCode::IoError,
            "Accessibility hierarchy result is invalid",
        )
    })?;
    Ok(VisualHierarchySnapshot {
        display: VisualDisplaySnapshot {
            display: wire.display,
            display_generation: wire.display_generation,
        },
        foreground: wire.foreground,
        nodes: wire.nodes,
        truncated: wire.truncated,
        proof: VisualSceneProof::Accessibility {
            component_generation: wire.component_generation,
            window_id: wire.window_id,
            scene_revision: wire.scene_revision,
            hierarchy_sha256: wire.hierarchy_sha256,
        },
    })
}

fn accessibility_proof(proof: VisualSceneProof) -> Result<serde_json::Value, ExecutionFailure> {
    match proof {
        VisualSceneProof::Accessibility {
            component_generation,
            window_id,
            scene_revision,
            hierarchy_sha256,
        } => Ok(serde_json::json!({
            "component_generation":component_generation,
            "window_id":window_id,
            "scene_revision":scene_revision,
            "hierarchy_sha256":hierarchy_sha256,
        })),
        _ => Err(stale("Accessibility interaction proof is invalid")),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EncodedImageWire {
    format: ImageFormat,
    mime: String,
    size: u64,
    width: u32,
    height: u32,
    #[serde(default)]
    display: Option<DisplayGeometry>,
    #[serde(default)]
    display_generation: Option<u64>,
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
    let wire: EncodedImageWire = serde_json::from_slice(&result.payload)
        .map_err(|_| DomainError::new(ErrorCode::IoError, "visual image result is invalid"))?;
    let expected_mime = match wire.format {
        ImageFormat::Heic => "image/heic",
        ImageFormat::Png => "image/png",
    };
    if wire.mime != expected_mime
        || wire.size == 0
        || wire.size > 8 * 1_024 * 1_024
        || wire.width == 0
        || wire.height == 0
        || wire.width > 16_384
        || wire.height > 16_384
    {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "visual image metadata is invalid",
        ));
    }
    let mut file = result.descriptors.remove(0).1;
    let bytes = read_bounded(&mut file, 8 * 1_024 * 1_024)?;
    if bytes.len() as u64 != wire.size {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "visual image size is invalid",
        ));
    }
    validate_encoded_bytes(wire.format, &bytes)?;
    if wire.format == ImageFormat::Png && png_dimensions(&bytes)? != (wire.width, wire.height) {
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

struct RawMetadata {
    width: u32,
    height: u32,
    pixel_format: u32,
    colorspace: u32,
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

    fn raw_metadata(&mut self) -> Result<RawMetadata, DomainError> {
        let mut writer = self.writer.take().ok_or_else(|| {
            DomainError::new(ErrorCode::InternalError, "visual raw writer is closed")
        })?;
        writer.flush().map_err(io_error)?;
        writer.sync_all().map_err(io_error)?;
        let size = writer.metadata().map_err(io_error)?.len();
        writer.seek(SeekFrom::Start(0)).map_err(io_error)?;
        let mut header = [0u8; 16];
        writer.read_exact(&mut header).map_err(io_error)?;
        let width = u32::from_le_bytes(header[0..4].try_into().unwrap());
        let height = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let pixel_format = u32::from_le_bytes(header[8..12].try_into().unwrap());
        let colorspace = u32::from_le_bytes(header[12..16].try_into().unwrap());
        let bytes_per_pixel = match pixel_format {
            1 | 2 | 5 => 4u64,
            3 => 3,
            4 => 2,
            _ => {
                return Err(DomainError::new(
                    ErrorCode::IoError,
                    "raw pixel format is unsupported",
                ));
            }
        };
        if width == 0 || height == 0 || width > 16_384 || height > 16_384 || colorspace > 2 {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "raw frame header is invalid",
            ));
        }
        let payload = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|value| value.checked_mul(bytes_per_pixel))
            .ok_or_else(|| DomainError::new(ErrorCode::ResourceLimit, "raw frame size overflow"))?;
        if payload > 67_108_864 || size != payload + 16 {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "raw frame length is invalid",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o400)).map_err(io_error)?;
        }
        Ok(RawMetadata {
            width,
            height,
            pixel_format,
            colorspace,
        })
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

fn validate_encoded_bytes(format: ImageFormat, bytes: &[u8]) -> Result<(), DomainError> {
    match format {
        ImageFormat::Png => png_dimensions(bytes).map(|_| ()),
        ImageFormat::Heic
            if bytes.len() >= 12
                && &bytes[4..8] == b"ftyp"
                && matches!(
                    &bytes[8..12],
                    b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1"
                ) =>
        {
            Ok(())
        }
        ImageFormat::Heic => Err(DomainError::new(
            ErrorCode::IoError,
            "HEIC image bytes are invalid",
        )),
    }
}

fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), DomainError> {
    if bytes.len() < 24
        || &bytes[..8] != b"\x89PNG\r\n\x1a\n"
        || &bytes[8..12] != 13u32.to_be_bytes().as_slice()
        || &bytes[12..16] != b"IHDR"
    {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "PNG image bytes are invalid",
        ));
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    if width == 0 || height == 0 {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "PNG dimensions are invalid",
        ));
    }
    Ok((width, height))
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
