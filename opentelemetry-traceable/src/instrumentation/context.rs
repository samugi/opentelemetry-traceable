use opentelemetry::Context;
use smallvec::SmallVec;

/// The slotted envelope carried inside an OpenTelemetry `Context` extension.
///
/// Holds (slot_number, span_context) tuples, used to identify the active span for
/// each slot.
///
/// This is used to hold the state of the in-process traces hierarchy.
/// The distributed slot is not handled here: the parent in that case is "current"
/// span, which `Context` has a dedicated field for.
#[derive(Clone)]
pub struct InProcessParents(pub SmallVec<[(u8, Context); 4]>);
