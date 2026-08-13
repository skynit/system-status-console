use libloading::Library;
use localdesk_remote_core::SecretValue;
use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::mem::MaybeUninit;
use std::ptr::{self, NonNull};
use std::sync::{Arc, OnceLock};

const LIBRARY_NAMES: &[&str] = &["libsmbclient.so.0", "libsmbclient.so"];
const MAX_DIRECTORY_ENTRIES: usize = 16 * 1024;
const MAX_ENTRY_NAME_BYTES: usize = 4 * 1024;
const DEFAULT_TIMEOUT_MS: c_int = 15_000;

const SMBC_DIR: c_uint = 7;
const SMBC_FILE: c_uint = 8;
const SMBC_LINK: c_uint = 9;
const SMBC_ENCRYPTLEVEL_DEFAULT: c_int = -1;
const SMBC_ENCRYPTLEVEL_REQUIRE: c_int = 2;

#[repr(C)]
struct SmbcContext {
    _private: [u8; 0],
}

#[repr(C)]
struct SmbcFile {
    _private: [u8; 0],
}

#[repr(C)]
struct SmbcDirent {
    smbc_type: c_uint,
    dirlen: c_uint,
    commentlen: c_uint,
    comment: *mut c_char,
    namelen: c_uint,
    name: [c_char; 1],
}

type AuthCallback = unsafe extern "C" fn(
    *mut SmbcContext,
    *const c_char,
    *const c_char,
    *mut c_char,
    c_int,
    *mut c_char,
    c_int,
    *mut c_char,
    c_int,
);
type NewContextFn = unsafe extern "C" fn() -> *mut SmbcContext;
type InitContextFn = unsafe extern "C" fn(*mut SmbcContext) -> *mut SmbcContext;
type FreeContextFn = unsafe extern "C" fn(*mut SmbcContext, c_int) -> c_int;
type SetAuthFn = unsafe extern "C" fn(*mut SmbcContext, Option<AuthCallback>);
type SetUserDataFn = unsafe extern "C" fn(*mut SmbcContext, *mut c_void);
type GetUserDataFn = unsafe extern "C" fn(*mut SmbcContext) -> *mut c_void;
type SetIntFn = unsafe extern "C" fn(*mut SmbcContext, c_int);
type SetPortFn = unsafe extern "C" fn(*mut SmbcContext, u16);
type SetProtocolsFn = unsafe extern "C" fn(*mut SmbcContext, *const c_char, *const c_char) -> c_int;

type StatFn = unsafe extern "C" fn(*mut SmbcContext, *const c_char, *mut libc::stat) -> c_int;
type OpendirFn = unsafe extern "C" fn(*mut SmbcContext, *const c_char) -> *mut SmbcFile;
type ClosedirFn = unsafe extern "C" fn(*mut SmbcContext, *mut SmbcFile) -> c_int;
type ReaddirFn = unsafe extern "C" fn(*mut SmbcContext, *mut SmbcFile) -> *mut SmbcDirent;
type MkdirFn = unsafe extern "C" fn(*mut SmbcContext, *const c_char, libc::mode_t) -> c_int;
type RmdirFn = unsafe extern "C" fn(*mut SmbcContext, *const c_char) -> c_int;
type UnlinkFn = unsafe extern "C" fn(*mut SmbcContext, *const c_char) -> c_int;
type RenameFn =
    unsafe extern "C" fn(*mut SmbcContext, *const c_char, *mut SmbcContext, *const c_char) -> c_int;
type OpenFn =
    unsafe extern "C" fn(*mut SmbcContext, *const c_char, c_int, libc::mode_t) -> *mut SmbcFile;
type ReadFn =
    unsafe extern "C" fn(*mut SmbcContext, *mut SmbcFile, *mut c_void, usize) -> libc::ssize_t;
type WriteFn =
    unsafe extern "C" fn(*mut SmbcContext, *mut SmbcFile, *const c_void, usize) -> libc::ssize_t;
type LseekFn =
    unsafe extern "C" fn(*mut SmbcContext, *mut SmbcFile, libc::off_t, c_int) -> libc::off_t;
type FstatFn = unsafe extern "C" fn(*mut SmbcContext, *mut SmbcFile, *mut libc::stat) -> c_int;
type CloseFn = unsafe extern "C" fn(*mut SmbcContext, *mut SmbcFile) -> c_int;

type GetStatFn = unsafe extern "C" fn(*mut SmbcContext) -> Option<StatFn>;
type GetOpendirFn = unsafe extern "C" fn(*mut SmbcContext) -> Option<OpendirFn>;
type GetClosedirFn = unsafe extern "C" fn(*mut SmbcContext) -> Option<ClosedirFn>;
type GetReaddirFn = unsafe extern "C" fn(*mut SmbcContext) -> Option<ReaddirFn>;
type GetMkdirFn = unsafe extern "C" fn(*mut SmbcContext) -> Option<MkdirFn>;
type GetRmdirFn = unsafe extern "C" fn(*mut SmbcContext) -> Option<RmdirFn>;
type GetUnlinkFn = unsafe extern "C" fn(*mut SmbcContext) -> Option<UnlinkFn>;
type GetRenameFn = unsafe extern "C" fn(*mut SmbcContext) -> Option<RenameFn>;
type GetOpenFn = unsafe extern "C" fn(*mut SmbcContext) -> Option<OpenFn>;
type GetReadFn = unsafe extern "C" fn(*mut SmbcContext) -> Option<ReadFn>;
type GetWriteFn = unsafe extern "C" fn(*mut SmbcContext) -> Option<WriteFn>;
type GetLseekFn = unsafe extern "C" fn(*mut SmbcContext) -> Option<LseekFn>;
type GetFstatFn = unsafe extern "C" fn(*mut SmbcContext) -> Option<FstatFn>;
type GetCloseFn = unsafe extern "C" fn(*mut SmbcContext) -> Option<CloseFn>;

struct Api {
    _library: Library,
    new_context: NewContextFn,
    init_context: InitContextFn,
    free_context: FreeContextFn,
    set_auth: SetAuthFn,
    set_user_data: SetUserDataFn,
    get_user_data: GetUserDataFn,
    set_timeout: SetIntFn,
    set_port: SetPortFn,
    set_url_encode_readdir: SetIntFn,
    set_one_share_per_server: SetIntFn,
    set_use_kerberos: SetIntFn,
    set_fallback_after_kerberos: SetIntFn,
    set_no_auto_anonymous: SetIntFn,
    set_use_ccache: SetIntFn,
    set_encryption: SetIntFn,
    set_protocols: SetProtocolsFn,
    get_stat: GetStatFn,
    get_opendir: GetOpendirFn,
    get_closedir: GetClosedirFn,
    get_readdir: GetReaddirFn,
    get_mkdir: GetMkdirFn,
    get_rmdir: GetRmdirFn,
    get_unlink: GetUnlinkFn,
    get_rename: GetRenameFn,
    get_open: GetOpenFn,
    get_read: GetReadFn,
    get_write: GetWriteFn,
    get_lseek: GetLseekFn,
    get_fstat: GetFstatFn,
    get_close: GetCloseFn,
}

static API: OnceLock<Arc<Api>> = OnceLock::new();

impl Api {
    fn load() -> Result<Arc<Self>, NativeError> {
        if let Some(api) = API.get() {
            return Ok(Arc::clone(api));
        }
        let library = LIBRARY_NAMES
            .iter()
            .find_map(|name| {
                // SAFETY: the library remains owned by `Api` for the process lifetime.
                unsafe { Library::new(name).ok() }
            })
            .ok_or_else(|| {
                NativeError::new(
                    NativeErrorKind::LibraryMissing,
                    "libsmbclient_not_installed",
                )
            })?;
        // SAFETY: every symbol type below matches the public libsmbclient ABI declared
        // by the installed `libsmbclient.h`; the retained `Library` outlives them.
        let api = unsafe {
            Arc::new(Self {
                new_context: load_symbol(&library, b"smbc_new_context\0")?,
                init_context: load_symbol(&library, b"smbc_init_context\0")?,
                free_context: load_symbol(&library, b"smbc_free_context\0")?,
                set_auth: load_symbol(&library, b"smbc_setFunctionAuthDataWithContext\0")?,
                set_user_data: load_symbol(&library, b"smbc_setOptionUserData\0")?,
                get_user_data: load_symbol(&library, b"smbc_getOptionUserData\0")?,
                set_timeout: load_symbol(&library, b"smbc_setTimeout\0")?,
                set_port: load_symbol(&library, b"smbc_setPort\0")?,
                set_url_encode_readdir: load_symbol(
                    &library,
                    b"smbc_setOptionUrlEncodeReaddirEntries\0",
                )?,
                set_one_share_per_server: load_symbol(
                    &library,
                    b"smbc_setOptionOneSharePerServer\0",
                )?,
                set_use_kerberos: load_symbol(&library, b"smbc_setOptionUseKerberos\0")?,
                set_fallback_after_kerberos: load_symbol(
                    &library,
                    b"smbc_setOptionFallbackAfterKerberos\0",
                )?,
                set_no_auto_anonymous: load_symbol(
                    &library,
                    b"smbc_setOptionNoAutoAnonymousLogin\0",
                )?,
                set_use_ccache: load_symbol(&library, b"smbc_setOptionUseCCache\0")?,
                set_encryption: load_symbol(&library, b"smbc_setOptionSmbEncryptionLevel\0")?,
                set_protocols: load_symbol(&library, b"smbc_setOptionProtocols\0")?,
                get_stat: load_symbol(&library, b"smbc_getFunctionStat\0")?,
                get_opendir: load_symbol(&library, b"smbc_getFunctionOpendir\0")?,
                get_closedir: load_symbol(&library, b"smbc_getFunctionClosedir\0")?,
                get_readdir: load_symbol(&library, b"smbc_getFunctionReaddir\0")?,
                get_mkdir: load_symbol(&library, b"smbc_getFunctionMkdir\0")?,
                get_rmdir: load_symbol(&library, b"smbc_getFunctionRmdir\0")?,
                get_unlink: load_symbol(&library, b"smbc_getFunctionUnlink\0")?,
                get_rename: load_symbol(&library, b"smbc_getFunctionRename\0")?,
                get_open: load_symbol(&library, b"smbc_getFunctionOpen\0")?,
                get_read: load_symbol(&library, b"smbc_getFunctionRead\0")?,
                get_write: load_symbol(&library, b"smbc_getFunctionWrite\0")?,
                get_lseek: load_symbol(&library, b"smbc_getFunctionLseek\0")?,
                get_fstat: load_symbol(&library, b"smbc_getFunctionFstat\0")?,
                get_close: load_symbol(&library, b"smbc_getFunctionClose\0")?,
                _library: library,
            })
        };
        if API.set(Arc::clone(&api)).is_err() {
            return Ok(Arc::clone(
                API.get().expect("SMB API initialized concurrently"),
            ));
        }
        Ok(api)
    }
}

unsafe fn load_symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T, NativeError> {
    // SAFETY: the caller supplies the exact ABI type for the named symbol.
    let symbol = unsafe { library.get::<T>(name) }.map_err(|_| {
        NativeError::new(
            NativeErrorKind::ApiIncompatible,
            "libsmbclient_api_incompatible",
        )
    })?;
    Ok(*symbol)
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum NativeErrorKind {
    LibraryMissing,
    ApiIncompatible,
    InvalidInput,
    Authentication,
    PermissionDenied,
    NotFound,
    Conflict,
    Timeout,
    Transport,
    Protocol,
    Limit,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct NativeError {
    pub(crate) kind: NativeErrorKind,
    pub(crate) reason: &'static str,
}

impl NativeError {
    const fn new(kind: NativeErrorKind, reason: &'static str) -> Self {
        Self { kind, reason }
    }

    fn from_errno(connecting: bool) -> Self {
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        match errno {
            libc::EACCES if connecting => Self::new(
                NativeErrorKind::Authentication,
                "smb_authentication_rejected",
            ),
            libc::EACCES | libc::EPERM => {
                Self::new(NativeErrorKind::PermissionDenied, "smb_permission_denied")
            }
            libc::ENOENT | libc::ENOTDIR => {
                Self::new(NativeErrorKind::NotFound, "smb_path_not_found")
            }
            libc::EEXIST | libc::ENOTEMPTY | libc::EBUSY => {
                Self::new(NativeErrorKind::Conflict, "smb_remote_conflict")
            }
            libc::ETIMEDOUT => Self::new(NativeErrorKind::Timeout, "smb_operation_timed_out"),
            libc::ECONNREFUSED
            | libc::ECONNRESET
            | libc::EHOSTDOWN
            | libc::EHOSTUNREACH
            | libc::ENETDOWN
            | libc::ENETUNREACH
            | libc::EPIPE => Self::new(NativeErrorKind::Transport, "smb_transport_failed"),
            libc::EINVAL | libc::ENAMETOOLONG => {
                Self::new(NativeErrorKind::InvalidInput, "smb_path_rejected")
            }
            _ => Self::new(NativeErrorKind::Protocol, "smb_protocol_operation_failed"),
        }
    }
}

pub(crate) enum NativeAuth {
    Password {
        username: String,
        domain: Option<String>,
        password: SecretValue,
    },
    Kerberos,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum NativeDialect {
    Smb2,
    Smb3,
}

pub(crate) struct NativeSmbConfig {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) share: String,
    pub(crate) dialect: NativeDialect,
    pub(crate) require_protection: bool,
    pub(crate) timeout_ms: u32,
    pub(crate) auth: NativeAuth,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum NativeEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct NativeEntry {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) kind: NativeEntryKind,
    pub(crate) size_bytes: Option<u64>,
    pub(crate) modified_at_unix_ms: Option<i64>,
    pub(crate) unix_mode: Option<u32>,
}

pub(crate) struct NativeReadChunk {
    pub(crate) before: NativeEntry,
    pub(crate) bytes: Vec<u8>,
    pub(crate) after: NativeEntry,
}

pub(crate) struct NativeWriteChunk {
    pub(crate) before: NativeEntry,
    pub(crate) after: NativeEntry,
}

pub(crate) trait SmbClient: Send {
    fn set_timeout_ms(&mut self, timeout_ms: u32);
    fn list(&mut self, path: &str) -> Result<Vec<NativeEntry>, NativeError>;
    fn stat(&mut self, path: &str) -> Result<NativeEntry, NativeError>;
    fn read_chunk(
        &mut self,
        path: &str,
        offset: u64,
        max_bytes: u32,
    ) -> Result<NativeReadChunk, NativeError>;
    fn prepare_write(&mut self, path: &str, resume: bool) -> Result<NativeEntry, NativeError>;
    fn write_chunk(
        &mut self,
        path: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<NativeWriteChunk, NativeError>;
    fn create_directory(&mut self, path: &str) -> Result<NativeEntry, NativeError>;
    fn rename(&mut self, from: &str, to: &str) -> Result<NativeEntry, NativeError>;
    fn delete(&mut self, path: &str) -> Result<(), NativeError>;
    fn disconnect(&mut self);
}

pub(crate) trait SmbConnector: Send + Sync {
    fn connect(&self, config: NativeSmbConfig) -> Result<Box<dyn SmbClient>, NativeError>;
}

pub(crate) struct NativeSmbConnector {
    api: Arc<Api>,
}

impl NativeSmbConnector {
    pub(crate) fn load() -> Result<Self, NativeError> {
        Api::load().map(|api| Self { api })
    }
}

impl SmbConnector for NativeSmbConnector {
    fn connect(&self, config: NativeSmbConfig) -> Result<Box<dyn SmbClient>, NativeError> {
        NativeSmbClient::connect(Arc::clone(&self.api), config)
            .map(|client| Box::new(client) as Box<dyn SmbClient>)
    }
}

struct NativeCredentials {
    domain: Vec<u8>,
    username: Vec<u8>,
    password: Vec<u8>,
}

impl NativeCredentials {
    fn from_auth(auth: NativeAuth) -> Result<(Self, bool), NativeError> {
        match auth {
            NativeAuth::Password {
                username,
                domain,
                password,
            } => Ok((
                Self {
                    domain: nul_terminated(domain.as_deref().unwrap_or(""))?,
                    username: nul_terminated(&username)?,
                    password: nul_terminated_bytes(password.expose_secret())?,
                },
                false,
            )),
            NativeAuth::Kerberos => Ok((
                Self {
                    domain: vec![0],
                    username: vec![0],
                    password: vec![0],
                },
                true,
            )),
        }
    }
}

impl Drop for NativeCredentials {
    fn drop(&mut self) {
        self.password.fill(0);
    }
}

struct NativeSmbClient {
    api: Arc<Api>,
    context: Option<NonNull<SmbcContext>>,
    credentials: Option<Box<NativeCredentials>>,
    base_uri: String,
    share_name: String,
}

// SAFETY: the raw context is never shared directly and every operation is
// serialized by the owning session mutex before this value crosses threads.
unsafe impl Send for NativeSmbClient {}

impl NativeSmbClient {
    fn connect(api: Arc<Api>, config: NativeSmbConfig) -> Result<Self, NativeError> {
        validate_share(&config.share)?;
        let base_uri = build_base_uri(&config.host, config.port, &config.share);
        let share_name = config.share.clone();
        let (credentials, kerberos) = NativeCredentials::from_auth(config.auth)?;
        let mut credentials = Box::new(credentials);

        // SAFETY: `new_context` is loaded from the matching library and takes no arguments.
        let raw = unsafe { (api.new_context)() };
        let context = NonNull::new(raw).ok_or_else(|| NativeError::from_errno(false))?;
        let mut guard = ContextGuard::new(Arc::clone(&api), context);
        let credentials_ptr = ptr::from_mut(credentials.as_mut()).cast::<c_void>();
        let min_protocol = match config.dialect {
            NativeDialect::Smb2 => c"SMB2",
            NativeDialect::Smb3 => c"SMB3",
        };

        // SAFETY: the context is live; all pointers remain valid through initialization.
        unsafe {
            (api.set_user_data)(context.as_ptr(), credentials_ptr);
            (api.set_auth)(context.as_ptr(), Some(auth_callback));
            (api.set_timeout)(
                context.as_ptr(),
                c_int::try_from(config.timeout_ms)
                    .unwrap_or(DEFAULT_TIMEOUT_MS)
                    .max(1),
            );
            (api.set_port)(context.as_ptr(), config.port);
            (api.set_url_encode_readdir)(context.as_ptr(), 0);
            (api.set_one_share_per_server)(context.as_ptr(), 0);
            (api.set_no_auto_anonymous)(context.as_ptr(), 1);
            (api.set_use_kerberos)(context.as_ptr(), c_int::from(kerberos));
            (api.set_use_ccache)(context.as_ptr(), c_int::from(kerberos));
            (api.set_fallback_after_kerberos)(context.as_ptr(), 0);
            (api.set_encryption)(
                context.as_ptr(),
                if config.require_protection {
                    SMBC_ENCRYPTLEVEL_REQUIRE
                } else {
                    SMBC_ENCRYPTLEVEL_DEFAULT
                },
            );
            if (api.set_protocols)(context.as_ptr(), min_protocol.as_ptr(), c"SMB3".as_ptr()) == 0 {
                return Err(NativeError::new(
                    NativeErrorKind::ApiIncompatible,
                    "libsmbclient_protocol_policy_rejected",
                ));
            }
            let initialized = (api.init_context)(context.as_ptr());
            let initialized =
                NonNull::new(initialized).ok_or_else(|| NativeError::from_errno(false))?;
            guard.context = initialized;
        }

        let mut client = Self {
            api,
            context: Some(guard.disarm()),
            credentials: Some(credentials),
            base_uri,
            share_name,
        };
        client.stat_internal("/", true)?;
        Ok(client)
    }

    fn context(&self) -> Result<NonNull<SmbcContext>, NativeError> {
        self.context
            .ok_or_else(|| NativeError::new(NativeErrorKind::Transport, "smb_session_disconnected"))
    }

    fn uri(&self, path: &str) -> Result<CString, NativeError> {
        let normalized = normalize_path(path)?;
        let mut uri = self.base_uri.clone();
        if normalized != "/" {
            for segment in normalized.trim_start_matches('/').split('/') {
                uri.push('/');
                uri.push_str(&percent_encode(segment.as_bytes()));
            }
        } else {
            uri.push('/');
        }
        CString::new(uri)
            .map_err(|_| NativeError::new(NativeErrorKind::InvalidInput, "smb_path_rejected"))
    }

    fn stat_internal(&mut self, path: &str, connecting: bool) -> Result<NativeEntry, NativeError> {
        let context = self.context()?;
        let uri = self.uri(path)?;
        // SAFETY: the context is initialized and the getter belongs to its library.
        let stat_fn = unsafe { (self.api.get_stat)(context.as_ptr()) }.ok_or_else(|| {
            NativeError::new(
                NativeErrorKind::ApiIncompatible,
                "libsmbclient_stat_unavailable",
            )
        })?;
        let mut stat = MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: the URI is NUL-terminated and `stat` points to writable storage.
        if unsafe { stat_fn(context.as_ptr(), uri.as_ptr(), stat.as_mut_ptr()) } < 0 {
            return Err(NativeError::from_errno(connecting));
        }
        // SAFETY: successful `stat_fn` initialized the complete `libc::stat` value.
        let stat = unsafe { stat.assume_init() };
        Ok(entry_from_stat(
            path,
            if path == "/" {
                &self.share_name
            } else {
                path_name(path)?
            },
            &stat,
        ))
    }

    fn close_directory(&self, context: NonNull<SmbcContext>, directory: NonNull<SmbcFile>) {
        // SAFETY: both handles belong to this live context. Close failure has no useful
        // recovery path after directory iteration and must not replace an earlier result.
        unsafe {
            if let Some(close) = (self.api.get_closedir)(context.as_ptr()) {
                let _ = close(context.as_ptr(), directory.as_ptr());
            }
        }
    }

    fn with_file<T>(
        &self,
        path: &str,
        flags: c_int,
        mode: libc::mode_t,
        operation: impl FnOnce(NonNull<SmbcContext>, NonNull<SmbcFile>) -> Result<T, NativeError>,
    ) -> Result<T, NativeError> {
        let context = self.context()?;
        let uri = self.uri(path)?;
        // SAFETY: the getter belongs to the initialized context.
        let open = unsafe { (self.api.get_open)(context.as_ptr()) }.ok_or_else(|| {
            NativeError::new(
                NativeErrorKind::ApiIncompatible,
                "libsmbclient_open_unavailable",
            )
        })?;
        // SAFETY: URI is NUL-terminated and the mode is used only with O_CREAT.
        let file = NonNull::new(unsafe { open(context.as_ptr(), uri.as_ptr(), flags, mode) })
            .ok_or_else(|| NativeError::from_errno(false))?;
        let result = operation(context, file);
        // SAFETY: the file was returned by this context and is closed exactly once.
        let close_result = unsafe { (self.api.get_close)(context.as_ptr()) }
            .ok_or_else(|| {
                NativeError::new(
                    NativeErrorKind::ApiIncompatible,
                    "libsmbclient_close_unavailable",
                )
            })
            .and_then(|close| {
                if unsafe { close(context.as_ptr(), file.as_ptr()) } < 0 {
                    Err(NativeError::from_errno(false))
                } else {
                    Ok(())
                }
            });
        match result {
            Err(error) => Err(error),
            Ok(value) => close_result.map(|()| value),
        }
    }

    fn file_entry(
        &self,
        context: NonNull<SmbcContext>,
        file: NonNull<SmbcFile>,
        path: &str,
    ) -> Result<NativeEntry, NativeError> {
        // SAFETY: the getter belongs to the initialized context.
        let fstat = unsafe { (self.api.get_fstat)(context.as_ptr()) }.ok_or_else(|| {
            NativeError::new(
                NativeErrorKind::ApiIncompatible,
                "libsmbclient_fstat_unavailable",
            )
        })?;
        let mut stat = MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: the file is live and stat points to writable storage.
        if unsafe { fstat(context.as_ptr(), file.as_ptr(), stat.as_mut_ptr()) } < 0 {
            return Err(NativeError::from_errno(false));
        }
        // SAFETY: successful fstat initialized the complete value.
        let stat = unsafe { stat.assume_init() };
        Ok(entry_from_stat(path, path_name(path)?, &stat))
    }

    fn seek_file(
        &self,
        context: NonNull<SmbcContext>,
        file: NonNull<SmbcFile>,
        offset: u64,
    ) -> Result<(), NativeError> {
        let offset = libc::off_t::try_from(offset).map_err(|_| {
            NativeError::new(NativeErrorKind::Limit, "smb_file_offset_exceeds_platform")
        })?;
        // SAFETY: the getter belongs to the initialized context.
        let seek = unsafe { (self.api.get_lseek)(context.as_ptr()) }.ok_or_else(|| {
            NativeError::new(
                NativeErrorKind::ApiIncompatible,
                "libsmbclient_lseek_unavailable",
            )
        })?;
        // SAFETY: the file is live and SEEK_SET uses the validated non-negative offset.
        if unsafe { seek(context.as_ptr(), file.as_ptr(), offset, libc::SEEK_SET) } != offset {
            return Err(NativeError::from_errno(false));
        }
        Ok(())
    }
}

impl SmbClient for NativeSmbClient {
    fn set_timeout_ms(&mut self, timeout_ms: u32) {
        let timeout_ms = c_int::try_from(timeout_ms).unwrap_or(c_int::MAX).max(1);
        if let Ok(context) = self.context() {
            // SAFETY: the context is initialized and this setter accepts any positive timeout.
            unsafe { (self.api.set_timeout)(context.as_ptr(), timeout_ms) };
        }
    }

    fn list(&mut self, path: &str) -> Result<Vec<NativeEntry>, NativeError> {
        let context = self.context()?;
        let uri = self.uri(path)?;
        // SAFETY: getters are invoked with this initialized context.
        let (open, read) = unsafe {
            (
                (self.api.get_opendir)(context.as_ptr()).ok_or_else(|| {
                    NativeError::new(
                        NativeErrorKind::ApiIncompatible,
                        "libsmbclient_opendir_unavailable",
                    )
                })?,
                (self.api.get_readdir)(context.as_ptr()).ok_or_else(|| {
                    NativeError::new(
                        NativeErrorKind::ApiIncompatible,
                        "libsmbclient_readdir_unavailable",
                    )
                })?,
            )
        };
        // SAFETY: URI is NUL-terminated and valid for the duration of this call.
        let directory = NonNull::new(unsafe { open(context.as_ptr(), uri.as_ptr()) })
            .ok_or_else(|| NativeError::from_errno(false))?;
        let mut entries = Vec::new();
        loop {
            // SAFETY: Linux exposes thread-local errno through this pointer.
            unsafe { *libc::__errno_location() = 0 };
            // SAFETY: directory is live until the matching closedir below.
            let raw = unsafe { read(context.as_ptr(), directory.as_ptr()) };
            let Some(dirent) = NonNull::new(raw) else {
                let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                if errno != 0 {
                    self.close_directory(context, directory);
                    return Err(NativeError::from_errno(false));
                }
                break;
            };
            if entries.len() >= MAX_DIRECTORY_ENTRIES {
                self.close_directory(context, directory);
                return Err(NativeError::new(
                    NativeErrorKind::Limit,
                    "smb_directory_entry_limit_exceeded",
                ));
            }
            // SAFETY: libsmbclient returns a live `smbc_dirent` until the next read.
            let dirent = unsafe { dirent.as_ref() };
            let name_len = usize::try_from(dirent.namelen).unwrap_or(usize::MAX);
            if name_len == 0 || name_len > MAX_ENTRY_NAME_BYTES {
                self.close_directory(context, directory);
                return Err(NativeError::new(
                    NativeErrorKind::Protocol,
                    "smb_entry_name_invalid",
                ));
            }
            // SAFETY: `namelen` is supplied for the flexible array member by libsmbclient.
            let name_bytes = unsafe {
                std::slice::from_raw_parts(ptr::addr_of!(dirent.name).cast::<u8>(), name_len)
            };
            let name = std::str::from_utf8(name_bytes).map_err(|_| {
                NativeError::new(NativeErrorKind::Protocol, "smb_entry_name_not_utf8")
            })?;
            if name == "." || name == ".." {
                continue;
            }
            validate_entry_name(name)?;
            let child_path = join_path(path, name)?;
            let mut entry = self.stat_internal(&child_path, false)?;
            entry.kind = match dirent.smbc_type {
                SMBC_DIR => NativeEntryKind::Directory,
                SMBC_FILE => NativeEntryKind::File,
                SMBC_LINK => NativeEntryKind::Symlink,
                _ => entry.kind,
            };
            entries.push(entry);
        }
        self.close_directory(context, directory);
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    fn stat(&mut self, path: &str) -> Result<NativeEntry, NativeError> {
        self.stat_internal(path, false)
    }

    fn read_chunk(
        &mut self,
        path: &str,
        offset: u64,
        max_bytes: u32,
    ) -> Result<NativeReadChunk, NativeError> {
        if max_bytes == 0 {
            return Err(NativeError::new(
                NativeErrorKind::InvalidInput,
                "smb_read_chunk_empty",
            ));
        }
        self.with_file(path, libc::O_RDONLY, 0, |context, file| {
            let before = self.file_entry(context, file, path)?;
            if before.kind != NativeEntryKind::File {
                return Err(NativeError::new(
                    NativeErrorKind::InvalidInput,
                    "smb_read_requires_regular_file",
                ));
            }
            if before.size_bytes.is_some_and(|size| offset > size) {
                return Err(NativeError::new(
                    NativeErrorKind::Conflict,
                    "smb_read_offset_exceeds_size",
                ));
            }
            self.seek_file(context, file, offset)?;
            // SAFETY: the getter belongs to the initialized context.
            let read = unsafe { (self.api.get_read)(context.as_ptr()) }.ok_or_else(|| {
                NativeError::new(
                    NativeErrorKind::ApiIncompatible,
                    "libsmbclient_read_unavailable",
                )
            })?;
            let maximum = usize::try_from(max_bytes).unwrap_or(usize::MAX);
            let mut bytes = Vec::with_capacity(maximum);
            let mut buffer = [0_u8; 64 * 1024];
            while bytes.len() < maximum {
                let requested = buffer.len().min(maximum - bytes.len());
                // SAFETY: buffer exposes `requested` writable bytes and file is live.
                let count = unsafe {
                    read(
                        context.as_ptr(),
                        file.as_ptr(),
                        buffer.as_mut_ptr().cast::<c_void>(),
                        requested,
                    )
                };
                if count < 0 {
                    return Err(NativeError::from_errno(false));
                }
                let count = usize::try_from(count).map_err(|_| {
                    NativeError::new(NativeErrorKind::Protocol, "smb_read_count_invalid")
                })?;
                if count == 0 {
                    break;
                }
                if count > requested {
                    return Err(NativeError::new(
                        NativeErrorKind::Protocol,
                        "smb_read_count_invalid",
                    ));
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            let after = self.file_entry(context, file, path)?;
            Ok(NativeReadChunk {
                before,
                bytes,
                after,
            })
        })
    }

    fn prepare_write(&mut self, path: &str, resume: bool) -> Result<NativeEntry, NativeError> {
        // libsmbclient fstat identity checks require a readable file handle.
        let flags = if resume {
            libc::O_RDWR
        } else {
            libc::O_RDWR | libc::O_CREAT | libc::O_TRUNC
        };
        self.with_file(path, flags, 0o660, |context, file| {
            self.file_entry(context, file, path)
        })
    }

    fn write_chunk(
        &mut self,
        path: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<NativeWriteChunk, NativeError> {
        if bytes.is_empty() {
            return Err(NativeError::new(
                NativeErrorKind::InvalidInput,
                "smb_write_chunk_empty",
            ));
        }
        self.with_file(path, libc::O_RDWR, 0, |context, file| {
            let before = self.file_entry(context, file, path)?;
            if before.kind != NativeEntryKind::File {
                return Err(NativeError::new(
                    NativeErrorKind::InvalidInput,
                    "smb_write_requires_regular_file",
                ));
            }
            self.seek_file(context, file, offset)?;
            // SAFETY: the getter belongs to the initialized context.
            let write = unsafe { (self.api.get_write)(context.as_ptr()) }.ok_or_else(|| {
                NativeError::new(
                    NativeErrorKind::ApiIncompatible,
                    "libsmbclient_write_unavailable",
                )
            })?;
            let mut written = 0_usize;
            while written < bytes.len() {
                let remaining = &bytes[written..];
                // SAFETY: the buffer is readable for `remaining.len()` and file is live.
                let count = unsafe {
                    write(
                        context.as_ptr(),
                        file.as_ptr(),
                        remaining.as_ptr().cast::<c_void>(),
                        remaining.len(),
                    )
                };
                if count < 0 {
                    return Err(NativeError::from_errno(false));
                }
                let count = usize::try_from(count).map_err(|_| {
                    NativeError::new(NativeErrorKind::Protocol, "smb_write_count_invalid")
                })?;
                if count == 0 || count > remaining.len() {
                    return Err(NativeError::new(
                        NativeErrorKind::Protocol,
                        "smb_write_count_invalid",
                    ));
                }
                written += count;
            }
            let after = self.file_entry(context, file, path)?;
            Ok(NativeWriteChunk { before, after })
        })
    }

    fn create_directory(&mut self, path: &str) -> Result<NativeEntry, NativeError> {
        let context = self.context()?;
        let uri = self.uri(path)?;
        // SAFETY: getter and returned function belong to the live context.
        let mkdir = unsafe { (self.api.get_mkdir)(context.as_ptr()) }.ok_or_else(|| {
            NativeError::new(
                NativeErrorKind::ApiIncompatible,
                "libsmbclient_mkdir_unavailable",
            )
        })?;
        // SAFETY: URI is NUL-terminated; mode is a bounded POSIX creation mode.
        if unsafe { mkdir(context.as_ptr(), uri.as_ptr(), 0o770) } < 0 {
            return Err(NativeError::from_errno(false));
        }
        self.stat_internal(path, false)
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<NativeEntry, NativeError> {
        let context = self.context()?;
        let from_uri = self.uri(from)?;
        let to_uri = self.uri(to)?;
        // SAFETY: getter and returned function belong to the live context.
        let rename = unsafe { (self.api.get_rename)(context.as_ptr()) }.ok_or_else(|| {
            NativeError::new(
                NativeErrorKind::ApiIncompatible,
                "libsmbclient_rename_unavailable",
            )
        })?;
        // SAFETY: both URIs are NUL-terminated and both sides use this share context.
        if unsafe {
            rename(
                context.as_ptr(),
                from_uri.as_ptr(),
                context.as_ptr(),
                to_uri.as_ptr(),
            )
        } < 0
        {
            return Err(NativeError::from_errno(false));
        }
        self.stat_internal(to, false)
    }

    fn delete(&mut self, path: &str) -> Result<(), NativeError> {
        let entry = self.stat_internal(path, false)?;
        let context = self.context()?;
        let uri = self.uri(path)?;
        let result = match entry.kind {
            NativeEntryKind::Directory => {
                // SAFETY: getter belongs to the initialized context.
                let rmdir = unsafe { (self.api.get_rmdir)(context.as_ptr()) }.ok_or_else(|| {
                    NativeError::new(
                        NativeErrorKind::ApiIncompatible,
                        "libsmbclient_rmdir_unavailable",
                    )
                })?;
                // SAFETY: URI is NUL-terminated and context is live.
                unsafe { rmdir(context.as_ptr(), uri.as_ptr()) }
            }
            _ => {
                // SAFETY: getter belongs to the initialized context.
                let unlink =
                    unsafe { (self.api.get_unlink)(context.as_ptr()) }.ok_or_else(|| {
                        NativeError::new(
                            NativeErrorKind::ApiIncompatible,
                            "libsmbclient_unlink_unavailable",
                        )
                    })?;
                // SAFETY: URI is NUL-terminated and context is live.
                unsafe { unlink(context.as_ptr(), uri.as_ptr()) }
            }
        };
        if result < 0 {
            return Err(NativeError::from_errno(false));
        }
        Ok(())
    }

    fn disconnect(&mut self) {
        if let Some(context) = self.context.take() {
            // SAFETY: context was allocated by this API and is taken exactly once.
            unsafe {
                (self.api.set_user_data)(context.as_ptr(), ptr::null_mut());
                let _ = (self.api.free_context)(context.as_ptr(), 1);
            }
        }
        self.credentials.take();
    }
}

impl Drop for NativeSmbClient {
    fn drop(&mut self) {
        self.disconnect();
    }
}

struct ContextGuard {
    api: Arc<Api>,
    context: NonNull<SmbcContext>,
    armed: bool,
}

impl ContextGuard {
    fn new(api: Arc<Api>, context: NonNull<SmbcContext>) -> Self {
        Self {
            api,
            context,
            armed: true,
        }
    }

    fn disarm(mut self) -> NonNull<SmbcContext> {
        self.armed = false;
        self.context
    }
}

impl Drop for ContextGuard {
    fn drop(&mut self) {
        if self.armed {
            // SAFETY: guard uniquely owns this not-yet-transferred context.
            unsafe {
                let _ = (self.api.free_context)(self.context.as_ptr(), 1);
            }
        }
    }
}

unsafe extern "C" fn auth_callback(
    context: *mut SmbcContext,
    _server: *const c_char,
    _share: *const c_char,
    domain: *mut c_char,
    domain_len: c_int,
    username: *mut c_char,
    username_len: c_int,
    password: *mut c_char,
    password_len: c_int,
) {
    let Some(api) = API.get() else {
        return;
    };
    // SAFETY: callback is invoked by the retained library for a live context.
    let credentials = unsafe { (api.get_user_data)(context) }.cast::<NativeCredentials>();
    let Some(credentials) = NonNull::new(credentials) else {
        return;
    };
    // SAFETY: user data points to the boxed credentials retained by `NativeSmbClient`.
    let credentials = unsafe { credentials.as_ref() };
    // SAFETY: libsmbclient supplies writable buffers with the declared lengths.
    unsafe {
        copy_to_c_buffer(&credentials.domain, domain, domain_len);
        copy_to_c_buffer(&credentials.username, username, username_len);
        copy_to_c_buffer(&credentials.password, password, password_len);
    }
}

unsafe fn copy_to_c_buffer(source: &[u8], destination: *mut c_char, capacity: c_int) {
    if destination.is_null() || capacity <= 0 {
        return;
    }
    let capacity = usize::try_from(capacity).unwrap_or(0);
    let source_without_nul = source.strip_suffix(&[0]).unwrap_or(source);
    let length = source_without_nul.len().min(capacity.saturating_sub(1));
    // SAFETY: caller guarantees a writable destination of `capacity` bytes.
    unsafe {
        ptr::copy_nonoverlapping(
            source_without_nul.as_ptr(),
            destination.cast::<u8>(),
            length,
        );
        *destination.add(length) = 0;
    }
}

fn nul_terminated(value: &str) -> Result<Vec<u8>, NativeError> {
    nul_terminated_bytes(value.as_bytes())
}

fn nul_terminated_bytes(value: &[u8]) -> Result<Vec<u8>, NativeError> {
    if value.contains(&0) {
        return Err(NativeError::new(
            NativeErrorKind::InvalidInput,
            "smb_credential_contains_nul",
        ));
    }
    let mut bytes = Vec::with_capacity(value.len() + 1);
    bytes.extend_from_slice(value);
    bytes.push(0);
    Ok(bytes)
}

fn validate_share(share: &str) -> Result<(), NativeError> {
    if share.is_empty()
        || share.len() > 255
        || share == "."
        || share == ".."
        || share
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\' | '@'))
    {
        return Err(NativeError::new(
            NativeErrorKind::InvalidInput,
            "smb_share_invalid",
        ));
    }
    Ok(())
}

fn normalize_path(path: &str) -> Result<&str, NativeError> {
    if !path.starts_with('/') || path.len() > 16 * 1024 || path.contains('\0') {
        return Err(NativeError::new(
            NativeErrorKind::InvalidInput,
            "smb_path_rejected",
        ));
    }
    if path == "/" {
        return Ok(path);
    }
    if path.ends_with('/')
        || path.split('/').skip(1).any(|segment| {
            segment.is_empty()
                || segment == "."
                || segment == ".."
                || segment.chars().any(char::is_control)
        })
    {
        return Err(NativeError::new(
            NativeErrorKind::InvalidInput,
            "smb_path_rejected",
        ));
    }
    Ok(path)
}

fn validate_entry_name(name: &str) -> Result<(), NativeError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > MAX_ENTRY_NAME_BYTES
        || name
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\'))
    {
        return Err(NativeError::new(
            NativeErrorKind::Protocol,
            "smb_entry_name_invalid",
        ));
    }
    Ok(())
}

fn build_base_uri(host: &str, port: u16, share: &str) -> String {
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    format!("smb://{host}:{port}/{}", percent_encode(share.as_bytes()))
}

fn percent_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(bytes.len());
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

fn join_path(parent: &str, name: &str) -> Result<String, NativeError> {
    normalize_path(parent)?;
    validate_entry_name(name)?;
    if parent == "/" {
        Ok(format!("/{name}"))
    } else {
        Ok(format!("{parent}/{name}"))
    }
}

fn path_name(path: &str) -> Result<&str, NativeError> {
    normalize_path(path)?;
    path.rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| NativeError::new(NativeErrorKind::InvalidInput, "smb_path_rejected"))
}

fn entry_from_stat(path: &str, name: &str, stat: &libc::stat) -> NativeEntry {
    let file_type = stat.st_mode & libc::S_IFMT;
    let kind = if file_type == libc::S_IFDIR {
        NativeEntryKind::Directory
    } else if file_type == libc::S_IFREG {
        NativeEntryKind::File
    } else if file_type == libc::S_IFLNK {
        NativeEntryKind::Symlink
    } else {
        NativeEntryKind::Other
    };
    NativeEntry {
        name: name.to_owned(),
        path: path.to_owned(),
        kind,
        size_bytes: u64::try_from(stat.st_size).ok(),
        modified_at_unix_ms: timestamp_ms(stat.st_mtime, stat.st_mtime_nsec),
        unix_mode: Some(stat.st_mode),
    }
}

fn timestamp_ms(seconds: libc::time_t, nanoseconds: libc::c_long) -> Option<i64> {
    let seconds = i128::from(seconds);
    let nanoseconds = i128::from(nanoseconds);
    if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
        return None;
    }
    let milliseconds = seconds
        .checked_mul(1_000)?
        .checked_add(nanoseconds / 1_000_000)?;
    i64::try_from(milliseconds).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_encoding_keeps_share_and_paths_inside_the_selected_authority() {
        assert_eq!(
            build_base_uri("2001:db8::1", 445, "团队 文档"),
            "smb://[2001:db8::1]:445/%E5%9B%A2%E9%98%9F%20%E6%96%87%E6%A1%A3"
        );
        assert_eq!(
            percent_encode("季度 1".as_bytes()),
            "%E5%AD%A3%E5%BA%A6%201"
        );
    }

    #[test]
    fn remote_paths_reject_parent_segments_and_ambiguous_separators() {
        assert!(normalize_path("/").is_ok());
        assert!(normalize_path("/reports/2026").is_ok());
        assert!(normalize_path("reports").is_err());
        assert!(normalize_path("/reports/../secret").is_err());
        assert!(normalize_path("/reports//secret").is_err());
        assert!(normalize_path("/reports/").is_err());
    }

    #[test]
    fn credential_callback_truncates_and_always_terminates() {
        let source = b"secret\0";
        let mut destination = [0x55_i8; 4];
        // SAFETY: destination is a writable four-byte buffer.
        unsafe { copy_to_c_buffer(source, destination.as_mut_ptr(), 4) };
        assert_eq!(destination.map(|value| value as u8), *b"sec\0");
    }
}
