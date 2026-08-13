use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const TELEMETRY_SCHEMA_VERSION: u16 = 4;
pub const TELEMETRY_SCOPE_SAME_EUID: &str = "same_euid";
pub const TELEMETRY_SCOPE_FULL_CGROUP: &str = "full_cgroup";
pub const TELEMETRY_SCOPE_SYSTEM: &str = "system";

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryStatus {
    Complete,
    Partial,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryFreshness {
    Fresh,
    WarmingUp,
    Stale,
    Unknown,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricState {
    Known,
    Unknown,
    PermissionDenied,
    Raced,
    Unbounded,
    WarmingUp,
    SamplingGap,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct MetricValue<T> {
    pub value: Option<T>,
    pub state: MetricState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl<T> MetricValue<T> {
    pub fn known(value: T) -> Self {
        Self {
            value: Some(value),
            state: MetricState::Known,
            reason: None,
        }
    }

    pub fn unavailable(state: MetricState, reason: impl Into<String>) -> Self {
        assert_ne!(
            state,
            MetricState::Known,
            "MetricValue::unavailable requires a non-Known state"
        );
        Self {
            value: None,
            state,
            reason: Some(reason.into()),
        }
    }

    pub fn is_known(&self) -> bool {
        self.state == MetricState::Known && self.value.is_some()
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupingResolution {
    DesktopEntryExact,
    CgroupScope,
    InheritedParent,
    Unknown,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct IssueCount {
    pub code: String,
    pub count: u64,
}

impl IssueCount {
    pub fn new(code: impl Into<String>, count: u64) -> Self {
        Self {
            code: code.into(),
            count,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct ApplicationSample {
    pub application_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desktop_entry_id: Option<String>,
    pub display_label: String,
    pub grouping_resolution: GroupingResolution,
    pub process_count: u64,
    pub process_scope: String,
    pub cgroup_scope: String,
    pub cpu_percent_total_capacity_sum: MetricValue<f64>,
    pub rss_sum_bytes: MetricValue<u64>,
    pub pss_sum_bytes: MetricValue<u64>,
    pub fd_used_sum: MetricValue<u64>,
    pub fd_soft_limit_sum: MetricValue<u64>,
    pub fd_percent_of_attributed_sum: MetricValue<f64>,
    pub fd_percent_of_soft_limit_sum: MetricValue<f64>,
    pub fd_max_process_percent_of_soft_limit: MetricValue<f64>,
    pub cgroup_cpu_percent_total_capacity: MetricValue<f64>,
    pub memory_current_bytes: MetricValue<u64>,
    pub cgroup_process_count: MetricValue<u64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct SystemFdSample {
    pub scope: String,
    pub file_nr_allocated: MetricValue<u64>,
    pub file_nr_max: MetricValue<u64>,
    pub file_max: MetricValue<u64>,
    pub pressure_percent: MetricValue<f64>,
}

impl SystemFdSample {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            scope: TELEMETRY_SCOPE_SYSTEM.to_owned(),
            file_nr_allocated: MetricValue::unavailable(MetricState::Unknown, reason.clone()),
            file_nr_max: MetricValue::unavailable(MetricState::Unknown, reason.clone()),
            file_max: MetricValue::unavailable(MetricState::Unknown, reason.clone()),
            pressure_percent: MetricValue::unavailable(MetricState::Unknown, reason),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct TelemetrySnapshot {
    pub schema_version: u16,
    pub snapshot_id: Uuid,
    pub captured_at_unix_ms: Option<i64>,
    pub sample_interval_ms: Option<u64>,
    pub logical_cpu_count: Option<u32>,
    pub freshness: TelemetryFreshness,
    pub status: TelemetryStatus,
    pub reason: String,
    pub retryable: bool,
    pub scope: String,
    pub last_success_at_unix_ms: Option<i64>,
    pub permission_denied_counts: Vec<IssueCount>,
    pub issues: Vec<IssueCount>,
    pub system_fd: SystemFdSample,
    pub applications: Vec<ApplicationSample>,
}

impl TelemetrySnapshot {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::unavailable_with_retryable(reason, true)
    }

    pub fn unavailable_with_retryable(reason: impl Into<String>, retryable: bool) -> Self {
        Self {
            schema_version: TELEMETRY_SCHEMA_VERSION,
            snapshot_id: Uuid::new_v4(),
            captured_at_unix_ms: None,
            sample_interval_ms: None,
            logical_cpu_count: None,
            freshness: TelemetryFreshness::Unknown,
            status: TelemetryStatus::Unavailable,
            reason: reason.into(),
            retryable,
            scope: TELEMETRY_SCOPE_SAME_EUID.to_owned(),
            last_success_at_unix_ms: None,
            permission_denied_counts: Vec::new(),
            issues: Vec::new(),
            system_fd: SystemFdSample::unavailable("telemetry_unavailable"),
            applications: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::ser::{
        Error as SerError, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant,
        SerializeTuple, SerializeTupleStruct, SerializeTupleVariant, Serializer,
    };
    use std::{cell::RefCell, fmt::Display, rc::Rc};

    #[test]
    fn schema_is_four_and_unavailable_metadata_is_unknown_not_zero() {
        let snapshot = TelemetrySnapshot::unavailable("procfs_unavailable");

        assert_eq!(TELEMETRY_SCHEMA_VERSION, 4);
        assert_eq!(snapshot.schema_version, 4);
        assert_eq!(snapshot.freshness, TelemetryFreshness::Unknown);
        assert_eq!(snapshot.status, TelemetryStatus::Unavailable);
        assert_eq!(snapshot.captured_at_unix_ms, None);
        assert_eq!(snapshot.sample_interval_ms, None);
        assert_eq!(snapshot.logical_cpu_count, None);
        assert_eq!(snapshot.last_success_at_unix_ms, None);
        assert!(snapshot.retryable);
    }

    #[test]
    fn public_snapshot_serialization_excludes_process_private_fields() {
        let snapshot = TelemetrySnapshot {
            schema_version: TELEMETRY_SCHEMA_VERSION,
            snapshot_id: Uuid::nil(),
            captured_at_unix_ms: None,
            sample_interval_ms: None,
            logical_cpu_count: None,
            freshness: TelemetryFreshness::WarmingUp,
            status: TelemetryStatus::Partial,
            reason: "warming_up".to_owned(),
            retryable: true,
            scope: TELEMETRY_SCOPE_SAME_EUID.to_owned(),
            last_success_at_unix_ms: None,
            permission_denied_counts: vec![IssueCount::new("rss_permission_denied", 1)],
            issues: vec![IssueCount::new("sampling_gap", 1)],
            applications: vec![ApplicationSample {
                application_key: "org.example.App".to_owned(),
                desktop_entry_id: Some("org.example.App.desktop".to_owned()),
                display_label: "Example App".to_owned(),
                grouping_resolution: GroupingResolution::DesktopEntryExact,
                process_count: 1,
                process_scope: TELEMETRY_SCOPE_SAME_EUID.to_owned(),
                cgroup_scope: TELEMETRY_SCOPE_FULL_CGROUP.to_owned(),
                cpu_percent_total_capacity_sum: MetricValue::known(12.5),
                rss_sum_bytes: MetricValue::known(4096),
                pss_sum_bytes: MetricValue::known(3072),
                fd_used_sum: MetricValue::known(4),
                fd_soft_limit_sum: MetricValue::known(1024),
                fd_percent_of_attributed_sum: MetricValue::known(50.0),
                fd_percent_of_soft_limit_sum: MetricValue::known(0.390625),
                fd_max_process_percent_of_soft_limit: MetricValue::known(90.0),
                cgroup_cpu_percent_total_capacity: MetricValue::known(10.0),
                memory_current_bytes: MetricValue::known(8192),
                cgroup_process_count: MetricValue::known(2),
            }],
            system_fd: SystemFdSample {
                scope: TELEMETRY_SCOPE_SYSTEM.to_owned(),
                file_nr_allocated: MetricValue::known(100),
                file_nr_max: MetricValue::known(0),
                file_max: MetricValue::known(1000),
                pressure_percent: MetricValue::known(10.0),
            },
        };
        let fields = capture_fields(&snapshot);

        assert_eq!(fields.value("captured_at_unix_ms").as_deref(), Some("null"));
        assert_eq!(fields.value("sample_interval_ms").as_deref(), Some("null"));
        assert_eq!(fields.value("logical_cpu_count").as_deref(), Some("null"));
        assert_eq!(
            fields.value("last_success_at_unix_ms").as_deref(),
            Some("null")
        );
        for nested_metric in [
            "cpu_percent_total_capacity_sum",
            "rss_sum_bytes",
            "pss_sum_bytes",
            "fd_used_sum",
            "fd_soft_limit_sum",
            "fd_percent_of_attributed_sum",
            "fd_percent_of_soft_limit_sum",
            "fd_max_process_percent_of_soft_limit",
            "cgroup_cpu_percent_total_capacity",
            "memory_current_bytes",
            "cgroup_process_count",
            "system_fd",
        ] {
            assert!(
                fields.contains_key(nested_metric),
                "nested application field: {nested_metric}"
            );
        }
        for private_key in [
            "boot_id",
            "pid",
            "ppid",
            "start_time_ticks",
            "euid",
            "comm",
            "exe",
            "cgroup",
            "identity",
            "processes",
            "ProcessIdentity",
            "ProcessSample",
        ] {
            assert!(
                !fields.contains_key(private_key),
                "private key: {private_key}"
            );
        }
    }

    #[test]
    fn metric_unknown_keeps_value_empty() {
        let metric = MetricValue::<u64>::unavailable(MetricState::PermissionDenied, "rss_denied");

        assert_eq!(metric.value, None);
        assert_eq!(metric.state, MetricState::PermissionDenied);
        assert_eq!(metric.reason.as_deref(), Some("rss_denied"));
    }

    #[test]
    fn metric_unavailable_rejects_known_state_and_accepts_typed_unknown_state() {
        let rejected = std::panic::catch_unwind(|| {
            MetricValue::<u64>::unavailable(MetricState::Known, "invalid_known_none")
        });
        assert!(rejected.is_err());

        let metric = MetricValue::<u64>::unavailable(MetricState::SamplingGap, "sampling_gap");
        assert_eq!(metric.value, None);
        assert_ne!(metric.state, MetricState::Known);
        assert_eq!(metric.reason.as_deref(), Some("sampling_gap"));
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct CaptureError;

    impl Display for CaptureError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("capture serializer error")
        }
    }

    impl std::error::Error for CaptureError {}

    impl SerError for CaptureError {
        fn custom<T: Display>(_msg: T) -> Self {
            Self
        }
    }

    #[derive(Clone, Default)]
    struct CapturedFields {
        entries: Rc<RefCell<Vec<(String, String)>>>,
    }

    impl CapturedFields {
        fn contains_key(&self, key: &str) -> bool {
            self.entries.borrow().iter().any(|(field, _)| field == key)
        }

        fn value(&self, key: &str) -> Option<String> {
            self.entries
                .borrow()
                .iter()
                .find(|(field, _)| field == key)
                .map(|(_, value)| value.clone())
        }
    }

    struct CaptureSerializer {
        fields: CapturedFields,
        output: Rc<RefCell<String>>,
    }

    impl CaptureSerializer {
        fn new(fields: CapturedFields) -> Self {
            Self {
                fields,
                output: Rc::new(RefCell::new(String::new())),
            }
        }

        fn write(&self, value: impl Into<String>) {
            *self.output.borrow_mut() = value.into();
        }
    }

    impl Serializer for &mut CaptureSerializer {
        type Ok = ();
        type Error = CaptureError;
        type SerializeSeq = CaptureSeq;
        type SerializeTuple = CaptureSeq;
        type SerializeTupleStruct = CaptureSeq;
        type SerializeTupleVariant = CaptureSeq;
        type SerializeMap = CaptureMap;
        type SerializeStruct = CaptureStruct;
        type SerializeStructVariant = CaptureStruct;

        fn serialize_bool(self, value: bool) -> Result<Self::Ok, Self::Error> {
            self.write(value.to_string());
            Ok(())
        }

        fn serialize_i8(self, value: i8) -> Result<Self::Ok, Self::Error> {
            self.write(value.to_string());
            Ok(())
        }

        fn serialize_i16(self, value: i16) -> Result<Self::Ok, Self::Error> {
            self.write(value.to_string());
            Ok(())
        }

        fn serialize_i32(self, value: i32) -> Result<Self::Ok, Self::Error> {
            self.write(value.to_string());
            Ok(())
        }

        fn serialize_i64(self, value: i64) -> Result<Self::Ok, Self::Error> {
            self.write(value.to_string());
            Ok(())
        }

        fn serialize_i128(self, value: i128) -> Result<Self::Ok, Self::Error> {
            self.write(value.to_string());
            Ok(())
        }

        fn serialize_u8(self, value: u8) -> Result<Self::Ok, Self::Error> {
            self.write(value.to_string());
            Ok(())
        }

        fn serialize_u16(self, value: u16) -> Result<Self::Ok, Self::Error> {
            self.write(value.to_string());
            Ok(())
        }

        fn serialize_u32(self, value: u32) -> Result<Self::Ok, Self::Error> {
            self.write(value.to_string());
            Ok(())
        }

        fn serialize_u64(self, value: u64) -> Result<Self::Ok, Self::Error> {
            self.write(value.to_string());
            Ok(())
        }

        fn serialize_u128(self, value: u128) -> Result<Self::Ok, Self::Error> {
            self.write(value.to_string());
            Ok(())
        }

        fn serialize_f32(self, value: f32) -> Result<Self::Ok, Self::Error> {
            self.write(value.to_string());
            Ok(())
        }

        fn serialize_f64(self, value: f64) -> Result<Self::Ok, Self::Error> {
            self.write(value.to_string());
            Ok(())
        }

        fn serialize_char(self, value: char) -> Result<Self::Ok, Self::Error> {
            self.write(format!("{value:?}"));
            Ok(())
        }

        fn serialize_str(self, value: &str) -> Result<Self::Ok, Self::Error> {
            self.write(format!("{value:?}"));
            Ok(())
        }

        fn serialize_bytes(self, value: &[u8]) -> Result<Self::Ok, Self::Error> {
            self.write(format!("{value:?}"));
            Ok(())
        }

        fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
            self.write("null");
            Ok(())
        }

        fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<Self::Ok, Self::Error> {
            value.serialize(self)
        }

        fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
            self.write("null");
            Ok(())
        }

        fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
            self.serialize_unit()
        }

        fn serialize_unit_variant(
            self,
            _name: &'static str,
            _variant_index: u32,
            variant: &'static str,
        ) -> Result<Self::Ok, Self::Error> {
            self.serialize_str(variant)
        }

        fn serialize_newtype_struct<T: ?Sized + Serialize>(
            self,
            _name: &'static str,
            value: &T,
        ) -> Result<Self::Ok, Self::Error> {
            value.serialize(self)
        }

        fn serialize_newtype_variant<T: ?Sized + Serialize>(
            self,
            _name: &'static str,
            _variant_index: u32,
            variant: &'static str,
            value: &T,
        ) -> Result<Self::Ok, Self::Error> {
            let fields = self.fields.clone();
            let mut nested = CaptureSerializer::new(fields);
            value.serialize(&mut nested)?;
            self.write(format!("{{{variant:?}:{}}}", nested.output.borrow()));
            Ok(())
        }

        fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
            Ok(CaptureSeq {
                fields: self.fields.clone(),
                output: self.output.clone(),
                values: Vec::new(),
            })
        }

        fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, Self::Error> {
            self.serialize_seq(None)
        }

        fn serialize_tuple_struct(
            self,
            _name: &'static str,
            _len: usize,
        ) -> Result<Self::SerializeTupleStruct, Self::Error> {
            self.serialize_seq(None)
        }

        fn serialize_tuple_variant(
            self,
            _name: &'static str,
            _variant_index: u32,
            _variant: &'static str,
            _len: usize,
        ) -> Result<Self::SerializeTupleVariant, Self::Error> {
            self.serialize_seq(None)
        }

        fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
            Ok(CaptureMap {
                fields: self.fields.clone(),
                output: self.output.clone(),
                entries: Vec::new(),
                pending_key: None,
            })
        }

        fn serialize_struct(
            self,
            _name: &'static str,
            _len: usize,
        ) -> Result<Self::SerializeStruct, Self::Error> {
            Ok(CaptureStruct {
                fields: self.fields.clone(),
                output: self.output.clone(),
                entries: Vec::new(),
            })
        }

        fn serialize_struct_variant(
            self,
            _name: &'static str,
            _variant_index: u32,
            _variant: &'static str,
            _len: usize,
        ) -> Result<Self::SerializeStructVariant, Self::Error> {
            self.serialize_struct("", 0)
        }
    }

    struct CaptureSeq {
        fields: CapturedFields,
        output: Rc<RefCell<String>>,
        values: Vec<String>,
    }

    impl CaptureSeq {
        fn push<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), CaptureError> {
            let mut serializer = CaptureSerializer::new(self.fields.clone());
            value.serialize(&mut serializer)?;
            self.values.push(serializer.output.borrow().clone());
            Ok(())
        }

        fn finish(self) -> Result<(), CaptureError> {
            *self.output.borrow_mut() = format!("[{}]", self.values.join(","));
            Ok(())
        }
    }

    impl SerializeSeq for CaptureSeq {
        type Ok = ();
        type Error = CaptureError;

        fn serialize_element<T: ?Sized + Serialize>(
            &mut self,
            value: &T,
        ) -> Result<(), Self::Error> {
            self.push(value)
        }

        fn end(self) -> Result<Self::Ok, Self::Error> {
            self.finish()
        }
    }

    impl SerializeTuple for CaptureSeq {
        type Ok = ();
        type Error = CaptureError;

        fn serialize_element<T: ?Sized + Serialize>(
            &mut self,
            value: &T,
        ) -> Result<(), Self::Error> {
            self.push(value)
        }

        fn end(self) -> Result<Self::Ok, Self::Error> {
            self.finish()
        }
    }

    impl SerializeTupleStruct for CaptureSeq {
        type Ok = ();
        type Error = CaptureError;

        fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
            self.push(value)
        }

        fn end(self) -> Result<Self::Ok, Self::Error> {
            self.finish()
        }
    }

    impl SerializeTupleVariant for CaptureSeq {
        type Ok = ();
        type Error = CaptureError;

        fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
            self.push(value)
        }

        fn end(self) -> Result<Self::Ok, Self::Error> {
            self.finish()
        }
    }

    struct CaptureMap {
        fields: CapturedFields,
        output: Rc<RefCell<String>>,
        entries: Vec<String>,
        pending_key: Option<String>,
    }

    impl SerializeMap for CaptureMap {
        type Ok = ();
        type Error = CaptureError;

        fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), Self::Error> {
            let mut serializer = CaptureSerializer::new(self.fields.clone());
            key.serialize(&mut serializer)?;
            self.pending_key = Some(serializer.output.borrow().clone());
            Ok(())
        }

        fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
            let mut serializer = CaptureSerializer::new(self.fields.clone());
            value.serialize(&mut serializer)?;
            self.entries.push(format!(
                "{}:{}",
                self.pending_key.take().unwrap_or_default(),
                serializer.output.borrow()
            ));
            Ok(())
        }

        fn end(self) -> Result<Self::Ok, Self::Error> {
            *self.output.borrow_mut() = format!("{{{}}}", self.entries.join(","));
            Ok(())
        }
    }

    struct CaptureStruct {
        fields: CapturedFields,
        output: Rc<RefCell<String>>,
        entries: Vec<String>,
    }

    impl CaptureStruct {
        fn field<T: ?Sized + Serialize>(
            &mut self,
            key: &'static str,
            value: &T,
        ) -> Result<(), CaptureError> {
            let mut serializer = CaptureSerializer::new(self.fields.clone());
            value.serialize(&mut serializer)?;
            let output = serializer.output.borrow().clone();
            self.fields
                .entries
                .borrow_mut()
                .push((key.to_owned(), output.clone()));
            self.entries.push(format!("{key:?}:{output}"));
            Ok(())
        }

        fn finish(self) -> Result<(), CaptureError> {
            *self.output.borrow_mut() = format!("{{{}}}", self.entries.join(","));
            Ok(())
        }
    }

    impl SerializeStruct for CaptureStruct {
        type Ok = ();
        type Error = CaptureError;

        fn serialize_field<T: ?Sized + Serialize>(
            &mut self,
            key: &'static str,
            value: &T,
        ) -> Result<(), Self::Error> {
            self.field(key, value)
        }

        fn end(self) -> Result<Self::Ok, Self::Error> {
            self.finish()
        }
    }

    impl SerializeStructVariant for CaptureStruct {
        type Ok = ();
        type Error = CaptureError;

        fn serialize_field<T: ?Sized + Serialize>(
            &mut self,
            key: &'static str,
            value: &T,
        ) -> Result<(), Self::Error> {
            self.field(key, value)
        }

        fn end(self) -> Result<Self::Ok, Self::Error> {
            self.finish()
        }
    }

    fn capture_fields<T: Serialize>(value: &T) -> CapturedFields {
        let fields = CapturedFields::default();
        let mut serializer = CaptureSerializer::new(fields.clone());
        value
            .serialize(&mut serializer)
            .expect("capture serialization");
        fields
    }
}
