use crate::{MAX_FILE_DESCRIPTORS, MAX_FRAME_BYTES, WireEnvelope};
use contract::ErrorCode;
use domain::DomainError;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    io::{Read, Write},
    mem::{offset_of, size_of, size_of_val},
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    os::unix::net::UnixStream,
};

#[derive(Debug)]
pub struct ReceivedEnvelope {
    pub envelope: WireEnvelope,
    pub descriptors: Vec<OwnedFd>,
}

pub fn connect_abstract(name: &str) -> std::io::Result<UnixStream> {
    if name.is_empty()
        || name.len() + 1 > size_of::<libc::sockaddr_un>() - offset_of!(libc::sockaddr_un, sun_path)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid abstract socket name",
        ));
    }
    let descriptor =
        unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, source) in address.sun_path[1..].iter_mut().zip(name.as_bytes()) {
        *target = *source as libc::c_char;
    }
    let address_length =
        (offset_of!(libc::sockaddr_un, sun_path) + 1 + name.len()) as libc::socklen_t;
    let result = unsafe {
        libc::connect(
            descriptor,
            std::ptr::addr_of!(address).cast(),
            address_length,
        )
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(descriptor) };
        return Err(error);
    }
    Ok(unsafe { UnixStream::from_raw_fd(descriptor) })
}

pub fn peer_uid(stream: &UnixStream) -> std::io::Result<u32> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(credentials).cast(),
            &mut length,
        )
    };
    if result != 0 || length as usize != size_of::<libc::ucred>() {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(credentials.uid)
    }
}

pub fn send_json<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<(), DomainError> {
    let body = serde_json::to_vec(value)
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "IPC JSON encoding failed"))?;
    send_body(stream, &body, &[])
}

pub fn receive_json<T: DeserializeOwned>(stream: &mut UnixStream) -> Result<T, DomainError> {
    let (body, descriptors) = receive_body(stream)?;
    if !descriptors.is_empty() {
        return Err(protocol_error("handshake cannot carry descriptors"));
    }
    serde_json::from_slice(&body).map_err(|_| protocol_error("invalid IPC JSON"))
}

pub fn send_envelope(
    stream: &mut UnixStream,
    envelope: &WireEnvelope,
    descriptors: &[RawFd],
) -> Result<(), DomainError> {
    envelope.validate(descriptors.len())?;
    let body = serde_json::to_vec(envelope)
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "IPC JSON encoding failed"))?;
    send_body(stream, &body, descriptors)
}

pub fn receive_envelope(stream: &mut UnixStream) -> Result<ReceivedEnvelope, DomainError> {
    let (body, descriptors) = receive_body(stream)?;
    let envelope: WireEnvelope =
        serde_json::from_slice(&body).map_err(|_| protocol_error("invalid IPC envelope JSON"))?;
    envelope.validate(descriptors.len())?;
    Ok(ReceivedEnvelope {
        envelope,
        descriptors,
    })
}

fn send_body(
    stream: &mut UnixStream,
    body: &[u8],
    descriptors: &[RawFd],
) -> Result<(), DomainError> {
    if body.is_empty() || body.len() > MAX_FRAME_BYTES || descriptors.len() > MAX_FILE_DESCRIPTORS {
        return Err(DomainError::new(
            ErrorCode::ResourceLimit,
            "IPC frame exceeds its bound",
        ));
    }
    let header = u32::try_from(body.len())
        .map_err(|_| DomainError::new(ErrorCode::ResourceLimit, "IPC frame length overflow"))?
        .to_be_bytes();
    if descriptors.is_empty() {
        stream.write_all(&header).map_err(io_error)?;
    } else {
        send_header_with_descriptors(stream.as_raw_fd(), &header, descriptors)?;
    }
    stream.write_all(body).map_err(io_error)
}

fn receive_body(stream: &mut UnixStream) -> Result<(Vec<u8>, Vec<OwnedFd>), DomainError> {
    let mut header = [0_u8; 4];
    let descriptors = receive_header_with_descriptors(stream.as_raw_fd(), &mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(protocol_error("invalid IPC frame length"));
    }
    let mut body = vec![0_u8; length];
    stream.read_exact(&mut body).map_err(io_error)?;
    std::str::from_utf8(&body).map_err(|_| protocol_error("IPC JSON is not UTF-8"))?;
    Ok((body, descriptors))
}

fn send_header_with_descriptors(
    socket: RawFd,
    header: &[u8; 4],
    descriptors: &[RawFd],
) -> Result<(), DomainError> {
    let mut iov = libc::iovec {
        iov_base: header.as_ptr().cast_mut().cast(),
        iov_len: header.len(),
    };
    let control_bytes = unsafe { libc::CMSG_SPACE(size_of_val(descriptors) as u32) as usize };
    let words = control_bytes.div_ceil(size_of::<usize>());
    let mut control = vec![0_usize; words];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = std::ptr::addr_of_mut!(iov);
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control_bytes;
    let first = unsafe { libc::CMSG_FIRSTHDR(std::ptr::addr_of!(message)) };
    if first.is_null() {
        return Err(protocol_error("cannot allocate descriptor control message"));
    }
    unsafe {
        (*first).cmsg_level = libc::SOL_SOCKET;
        (*first).cmsg_type = libc::SCM_RIGHTS;
        (*first).cmsg_len = libc::CMSG_LEN(size_of_val(descriptors) as u32) as usize;
        std::ptr::copy_nonoverlapping(
            descriptors.as_ptr(),
            libc::CMSG_DATA(first).cast::<RawFd>(),
            descriptors.len(),
        );
    }
    let sent = loop {
        let result =
            unsafe { libc::sendmsg(socket, std::ptr::addr_of!(message), libc::MSG_NOSIGNAL) };
        if result >= 0 {
            break result;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(io_error(error));
        }
    };
    if sent != header.len() as isize {
        return Err(protocol_error("partial descriptor frame header"));
    }
    Ok(())
}

fn receive_header_with_descriptors(
    socket: RawFd,
    header: &mut [u8; 4],
) -> Result<Vec<OwnedFd>, DomainError> {
    let mut iov = libc::iovec {
        iov_base: header.as_mut_ptr().cast(),
        iov_len: header.len(),
    };
    let control_bytes =
        unsafe { libc::CMSG_SPACE((MAX_FILE_DESCRIPTORS * size_of::<RawFd>()) as u32) as usize };
    let words = control_bytes.div_ceil(size_of::<usize>());
    let mut control = vec![0_usize; words];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = std::ptr::addr_of_mut!(iov);
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control_bytes;
    let received = loop {
        let result = unsafe {
            libc::recvmsg(
                socket,
                std::ptr::addr_of_mut!(message),
                libc::MSG_WAITALL | libc::MSG_CMSG_CLOEXEC,
            )
        };
        if result >= 0 {
            break result;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(io_error(error));
        }
    };
    if received == 0 {
        return Err(DomainError::new(
            ErrorCode::IoError,
            "IPC peer disconnected",
        ));
    }
    if received != header.len() as isize || message.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(protocol_error("truncated IPC frame header or descriptors"));
    }
    let mut descriptors = Vec::new();
    let mut current = unsafe { libc::CMSG_FIRSTHDR(std::ptr::addr_of!(message)) };
    while !current.is_null() {
        let header_ref = unsafe { &*current };
        if header_ref.cmsg_level != libc::SOL_SOCKET || header_ref.cmsg_type != libc::SCM_RIGHTS {
            return Err(protocol_error("unknown IPC ancillary message"));
        }
        let data_length = header_ref
            .cmsg_len
            .checked_sub(unsafe { libc::CMSG_LEN(0) } as usize)
            .ok_or_else(|| protocol_error("invalid IPC descriptor message"))?;
        if data_length % size_of::<RawFd>() != 0 {
            return Err(protocol_error("invalid IPC descriptor length"));
        }
        let count = data_length / size_of::<RawFd>();
        if descriptors.len() + count > MAX_FILE_DESCRIPTORS {
            return Err(protocol_error("too many IPC descriptors"));
        }
        let data = unsafe { libc::CMSG_DATA(current).cast::<RawFd>() };
        for index in 0..count {
            let descriptor = unsafe { *data.add(index) };
            if descriptor < 0 {
                return Err(protocol_error("invalid IPC descriptor"));
            }
            descriptors.push(unsafe { OwnedFd::from_raw_fd(descriptor) });
        }
        current = unsafe { libc::CMSG_NXTHDR(std::ptr::addr_of!(message), current) };
    }
    Ok(descriptors)
}

fn protocol_error(reason: &'static str) -> DomainError {
    DomainError::new(ErrorCode::ProtocolIncompatible, reason)
}

fn io_error(_: std::io::Error) -> DomainError {
    DomainError::new(ErrorCode::IoError, "daemon IPC I/O failed")
}
