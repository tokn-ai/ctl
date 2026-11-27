import { record, validAgentId } from "./model.js";

export interface LiveParticipant {
  participant_id: string;
  agent_id: string;
}

/** Fractions of the selected image's width and height, measured from the top left. */
type LivePoint = [x: number, y: number];

export type LiveAction =
  | {operation: "move" | "click" | "scroll" | "pointer_down" | "pointer_up"; point: LivePoint}
  | {operation: "drag"; from: LivePoint; to: LivePoint}
  | {operation: "press_keys" | "type_text" | "invoke" | "set_text"};

export type LiveEvent =
  | {type: "snapshot"; self_id: string; participants: LiveParticipant[]}
  | {type: "joined"; participant: LiveParticipant}
  | {type: "left"; participant_id: string}
  | {type: "action"; participant_id: string; action: LiveAction; action_id?: string}
  | {type: "turn"; holder: LiveParticipant; expires_at_ms: number}
  | {type: "turn"; holder: null; expires_at_ms: null};

function validId(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9-]{1,64}$/.test(value);
}

function participant(value: unknown): LiveParticipant {
  if (!record(value) || !validId(value.participant_id) || !validAgentId(value.agent_id)) {
    throw new Error("invalid ctream watch participant");
  }
  return {participant_id: value.participant_id, agent_id: value.agent_id};
}

function point(value: unknown): LivePoint {
  if (!Array.isArray(value) || value.length !== 2 ||
      !value.every((n) => typeof n === "number" && n >= 0 && n <= 1)) {
    throw new Error("invalid ctream watch coordinates");
  }
  return [value[0], value[1]];
}

function action(value: unknown): LiveAction {
  if (record(value)) {
    const {operation} = value;
    switch (operation) {
      case "move": case "click": case "scroll": case "pointer_down": case "pointer_up":
        return {operation, point: point(value.point)};
      case "drag":
        return {operation, from: point(value.from), to: point(value.to)};
      case "press_keys": case "type_text": case "invoke": case "set_text":
        return {operation};
    }
  }
  throw new Error("invalid ctream watch action");
}

export function liveEvent(value: unknown, turns: boolean): LiveEvent {
  if (record(value)) {
    switch (value.type) {
      case "snapshot":
        if (validId(value.self_id) && Array.isArray(value.participants)) {
          const participants = value.participants.map(participant);
          const ids = new Set(participants.map((member) => member.participant_id));
          if (ids.has(value.self_id) && ids.size === participants.length) {
            return {type: "snapshot", self_id: value.self_id, participants};
          }
        }
        break;
      case "joined":
        return {type: "joined", participant: participant(value.participant)};
      case "left":
        if (validId(value.participant_id)) return {type: "left", participant_id: value.participant_id};
        break;
      case "action":
        if (validId(value.participant_id)) {
          const event = {type: "action" as const, participant_id: value.participant_id, action: action(value.action)};
          if (!turns || value.action_id === undefined) return event;
          if (typeof value.action_id === "string" &&
              /^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/i.test(value.action_id)) {
            return {...event, action_id: value.action_id};
          }
        }
        break;
      case "turn":
        if (value.holder === null && value.expires_at_ms === null) {
          return {type: "turn", holder: null, expires_at_ms: null};
        }
        if (typeof value.expires_at_ms === "number" && Number.isSafeInteger(value.expires_at_ms) && value.expires_at_ms >= 0) {
          return {type: "turn", holder: participant(value.holder), expires_at_ms: value.expires_at_ms};
        }
    }
  }
  throw new Error("invalid ctream watch event");
}
