use std::collections::VecDeque;

use super::*;
use crate::model::glconst::*;

pub const MAX_DEBUG_MESSAGE_LENGTH_VALUE: usize = 1024;
pub const MAX_DEBUG_LOGGED_MESSAGES_VALUE: usize = 64;
pub const MAX_DEBUG_GROUP_STACK_DEPTH_VALUE: usize = 64;
pub const MAX_LABEL_LENGTH_VALUE: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugMessage {
    pub source: u32,
    pub type_: u32,
    pub id: u32,
    pub severity: u32,
    pub text: Vec<u8>,
}

#[derive(Clone)]
struct FilterRule {
    source: u32,
    type_: u32,
    severity: u32,
    ids: Vec<u32>,
    enabled: bool,
}

impl FilterRule {
    fn matches(&self, message: &DebugMessage) -> bool {
        (self.source == GL_DONT_CARE || self.source == message.source)
            && (self.type_ == GL_DONT_CARE || self.type_ == message.type_)
            && (self.severity == GL_DONT_CARE || self.severity == message.severity)
            && (self.ids.is_empty() || self.ids.contains(&message.id))
    }
}

#[derive(Clone)]
struct DebugGroup {
    source: u32,
    id: u32,
    message: Vec<u8>,
    filters: Vec<FilterRule>,
}

pub(crate) struct DebugState {
    callback: usize,
    user_param: usize,
    log: VecDeque<DebugMessage>,
    groups: Vec<DebugGroup>,
    context_flags: u32,
}

impl DebugState {
    pub(crate) fn new(debug_context: bool) -> Self {
        Self {
            callback: 0,
            user_param: 0,
            log: VecDeque::new(),
            groups: vec![DebugGroup {
                source: GL_DEBUG_SOURCE_APPLICATION,
                id: 0,
                message: Vec::new(),
                filters: Vec::new(),
            }],
            context_flags: if debug_context {
                GL_CONTEXT_FLAG_DEBUG_BIT
            } else {
                0
            },
        }
    }

    fn enabled(&self, message: &DebugMessage) -> bool {
        let default = message.severity != GL_DEBUG_SEVERITY_LOW;
        self.groups
            .last()
            .unwrap()
            .filters
            .iter()
            .fold(default, |enabled, rule| {
                if rule.matches(message) {
                    rule.enabled
                } else {
                    enabled
                }
            })
    }
}

pub enum DebugDelivery {
    Callback {
        callback: usize,
        user_param: usize,
        message: DebugMessage,
    },
    Logged,
    Discarded,
}

impl GlContext {
    pub fn debug_identifier_valid(identifier: u32) -> bool {
        matches!(
            identifier,
            GL_BUFFER_OBJECT
                | GL_SHADER_OBJECT
                | GL_PROGRAM_OBJECT
                | GL_VERTEX_ARRAY_OBJECT
                | GL_QUERY_OBJECT
                | GL_PROGRAM_PIPELINE_OBJECT
                | GL_TRANSFORM_FEEDBACK
                | GL_SAMPLER_OBJECT
                | GL_TEXTURE
                | GL_RENDERBUFFER
                | GL_FRAMEBUFFER
        )
    }

    pub fn debug_object_valid(&self, identifier: u32, name: u32) -> bool {
        let exists = match identifier {
            GL_BUFFER_OBJECT => self.is_buffer_name(name),
            GL_SHADER_OBJECT => self.programs.shader_exists(name),
            GL_PROGRAM_OBJECT => self.programs.contains(name),
            GL_VERTEX_ARRAY_OBJECT => self.is_vertex_array(name),
            GL_QUERY_OBJECT => self.is_query(name),
            GL_PROGRAM_PIPELINE_OBJECT => self.is_program_pipeline(name),
            GL_TRANSFORM_FEEDBACK => self.is_transform_feedback(name),
            GL_SAMPLER_OBJECT => {
                !self.pending_sampler_deletes.contains(&name) && self.samplers.known(name)
            }
            GL_TEXTURE => self.is_texture_name(name),
            GL_RENDERBUFFER => self.is_renderbuffer(name),
            GL_FRAMEBUFFER => self.has_framebuffer(name),
            _ => false,
        };
        if !exists {
            return false;
        }
        matches!(
            identifier,
            GL_SHADER_OBJECT | GL_PROGRAM_OBJECT | GL_SAMPLER_OBJECT
        ) || self.debug_object_materialized(identifier, name)
    }

    fn debug_object_materialized(&self, identifier: u32, name: u32) -> bool {
        if Self::local_label_namespace(identifier) {
            self.local.debug_materialized.contains(&(identifier, name))
        } else {
            self.debug_materialized.contains(&(identifier, name))
        }
    }

    pub fn mark_debug_object_materialized(&mut self, identifier: u32, name: u32) {
        if name == 0 {
            return;
        }
        if Self::local_label_namespace(identifier) {
            self.local.debug_materialized.insert((identifier, name));
        } else {
            self.debug_materialized.insert((identifier, name));
        }
    }
    pub fn set_debug_context(&mut self, debug: bool) {
        self.local.debug = DebugState::new(debug);
        self.local.pipeline.debug_output = debug;
    }

    pub fn context_flags(&self) -> u32 {
        self.local.debug.context_flags
    }
    pub fn debug_callback(&self) -> (usize, usize) {
        (self.local.debug.callback, self.local.debug.user_param)
    }
    pub fn set_debug_callback(&mut self, callback: usize, user_param: usize) {
        self.local.debug.callback = callback;
        self.local.debug.user_param = user_param;
    }
    pub fn debug_group_depth(&self) -> usize {
        self.local.debug.groups.len()
    }
    pub fn debug_group_can_push(&self) -> bool {
        self.local.debug.groups.len() < MAX_DEBUG_GROUP_STACK_DEPTH_VALUE
    }
    pub fn debug_log_len(&self) -> usize {
        self.local.debug.log.len()
    }
    pub fn next_debug_message_length(&self) -> usize {
        self.local
            .debug
            .log
            .front()
            .map_or(0, |message| message.text.len() + 1)
    }

    pub fn debug_message_control(
        &mut self,
        source: u32,
        type_: u32,
        severity: u32,
        ids: Vec<u32>,
        enabled: bool,
    ) {
        self.local
            .debug
            .groups
            .last_mut()
            .unwrap()
            .filters
            .push(FilterRule {
                source,
                type_,
                severity,
                ids,
                enabled,
            });
    }

    pub fn deliver_debug_message(&mut self, message: DebugMessage) -> DebugDelivery {
        if !self.local.pipeline.debug_output || !self.local.debug.enabled(&message) {
            return DebugDelivery::Discarded;
        }
        if self.local.debug.callback != 0 {
            return DebugDelivery::Callback {
                callback: self.local.debug.callback,
                user_param: self.local.debug.user_param,
                message,
            };
        }
        if self.local.debug.log.len() < MAX_DEBUG_LOGGED_MESSAGES_VALUE {
            self.local.debug.log.push_back(message);
            DebugDelivery::Logged
        } else {
            DebugDelivery::Discarded
        }
    }

    pub fn take_debug_message(&mut self) -> Option<DebugMessage> {
        self.local.debug.log.pop_front()
    }

    pub fn push_debug_group(&mut self, source: u32, id: u32, message: Vec<u8>) -> bool {
        if self.local.debug.groups.len() == MAX_DEBUG_GROUP_STACK_DEPTH_VALUE {
            self.set_gl_error(GL_STACK_OVERFLOW);
            return false;
        }
        let filters = self.local.debug.groups.last().unwrap().filters.clone();
        self.local.debug.groups.push(DebugGroup {
            source,
            id,
            message,
            filters,
        });
        true
    }

    pub fn pop_debug_group(&mut self) -> Option<DebugMessage> {
        if self.local.debug.groups.len() == 1 {
            self.set_gl_error(GL_STACK_UNDERFLOW);
            return None;
        }
        let group = self.local.debug.groups.pop().unwrap();
        Some(DebugMessage {
            source: group.source,
            type_: GL_DEBUG_TYPE_POP_GROUP,
            id: group.id,
            severity: GL_DEBUG_SEVERITY_NOTIFICATION,
            text: group.message,
        })
    }

    fn local_label_namespace(identifier: u32) -> bool {
        matches!(
            identifier,
            GL_VERTEX_ARRAY_OBJECT
                | GL_QUERY_OBJECT
                | GL_PROGRAM_PIPELINE_OBJECT
                | GL_TRANSFORM_FEEDBACK
                | GL_FRAMEBUFFER
        )
    }

    pub fn set_object_label(&mut self, identifier: u32, name: u32, label: Option<Vec<u8>>) {
        let labels = if Self::local_label_namespace(identifier) {
            &mut self.local.debug_labels
        } else {
            &mut self.debug_labels
        };
        if let Some(label) = label {
            labels.insert((identifier, name), label);
        } else {
            labels.remove(&(identifier, name));
        }
    }
    pub fn clear_object_label(&mut self, identifier: u32, name: u32) {
        if Self::local_label_namespace(identifier) {
            self.local.debug_labels.remove(&(identifier, name));
            self.local.debug_materialized.remove(&(identifier, name));
        } else {
            self.debug_labels.remove(&(identifier, name));
            self.debug_materialized.remove(&(identifier, name));
        }
    }
    pub fn object_label(&self, identifier: u32, name: u32) -> &[u8] {
        let labels = if Self::local_label_namespace(identifier) {
            &self.local.debug_labels
        } else {
            &self.debug_labels
        };
        labels.get(&(identifier, name)).map_or(&[], Vec::as_slice)
    }
    pub fn set_pointer_label(&mut self, pointer: usize, label: Option<Vec<u8>>) {
        if let Some(label) = label {
            self.debug_pointer_labels.insert(pointer, label);
        } else {
            self.debug_pointer_labels.remove(&pointer);
        }
    }
    pub fn pointer_label(&self, pointer: usize) -> &[u8] {
        self.debug_pointer_labels
            .get(&pointer)
            .map_or(&[], Vec::as_slice)
    }
}

impl Default for DebugState {
    fn default() -> Self {
        Self::new(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: u32, severity: u32) -> DebugMessage {
        DebugMessage {
            source: GL_DEBUG_SOURCE_APPLICATION,
            type_: GL_DEBUG_TYPE_MARKER,
            id,
            severity,
            text: format!("message-{id}").into_bytes(),
        }
    }

    #[test]
    fn debug_context_defaults_and_callback_are_context_state() {
        let mut context = GlContext::new();
        context.set_debug_context(true);
        assert_eq!(context.context_flags(), GL_CONTEXT_FLAG_DEBUG_BIT);
        assert!(context.is_enabled(GL_DEBUG_OUTPUT));
        assert!(!context.is_enabled(GL_DEBUG_OUTPUT_SYNCHRONOUS));
        context.set_debug_callback(0x1234, 0x5678);
        assert_eq!(context.debug_callback(), (0x1234, 0x5678));
        context.reset_frame();
        assert_eq!(context.debug_callback(), (0x1234, 0x5678));
        assert!(context.is_enabled(GL_DEBUG_OUTPUT));
    }

    #[test]
    fn filters_are_inherited_and_restored_by_groups() {
        let mut context = GlContext::new();
        context.enable(GL_DEBUG_OUTPUT);
        context.debug_message_control(GL_DONT_CARE, GL_DONT_CARE, GL_DONT_CARE, Vec::new(), false);
        assert!(matches!(
            context.deliver_debug_message(message(1, GL_DEBUG_SEVERITY_HIGH)),
            DebugDelivery::Discarded
        ));
        assert!(context.push_debug_group(GL_DEBUG_SOURCE_APPLICATION, 9, b"group".to_vec()));
        context.debug_message_control(
            GL_DEBUG_SOURCE_APPLICATION,
            GL_DEBUG_TYPE_MARKER,
            GL_DONT_CARE,
            vec![2],
            true,
        );
        assert!(matches!(
            context.deliver_debug_message(message(2, GL_DEBUG_SEVERITY_HIGH)),
            DebugDelivery::Logged
        ));
        context.pop_debug_group();
        assert!(matches!(
            context.deliver_debug_message(message(2, GL_DEBUG_SEVERITY_HIGH)),
            DebugDelivery::Discarded
        ));
    }

    #[test]
    fn group_bounds_and_labels_are_observable() {
        let mut context = GlContext::new();
        assert_eq!(context.debug_group_depth(), 1);
        assert!(context.pop_debug_group().is_none());
        assert_eq!(context.take_gl_error(), GL_STACK_UNDERFLOW);
        for id in 1..MAX_DEBUG_GROUP_STACK_DEPTH_VALUE {
            assert!(context.push_debug_group(
                GL_DEBUG_SOURCE_APPLICATION,
                id as u32,
                b"g".to_vec()
            ));
        }
        assert!(!context.push_debug_group(GL_DEBUG_SOURCE_APPLICATION, 99, b"overflow".to_vec()));
        assert_eq!(context.take_gl_error(), GL_STACK_OVERFLOW);
        context.set_object_label(GL_BUFFER_OBJECT, 7, Some(b"buffer-seven".to_vec()));
        assert_eq!(context.object_label(GL_BUFFER_OBJECT, 7), b"buffer-seven");
    }

    #[test]
    fn program_label_survives_delete_pending_and_clears_on_destruction() {
        let mut context = GlContext::new();
        let program = context.create_program();
        context.local.cur_prog = program;
        context.set_object_label(GL_PROGRAM_OBJECT, program, Some(b"pending".to_vec()));

        context.delete_program(program);
        assert!(context.programs.contains(program));
        assert_eq!(context.object_label(GL_PROGRAM_OBJECT, program), b"pending");

        context.local.cur_prog = 0;
        context.destroy_program(program);
        assert!(!context.programs.contains(program));
        assert_eq!(context.object_label(GL_PROGRAM_OBJECT, program), b"");
    }
}
