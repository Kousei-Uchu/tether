export interface PeerSummary {
  name: string;
  platform: string;
  fingerprint: string;
  /** On a trust list: what the device may do here. On a connection: what the host allows this device to do. */
  permissions: string[];
}

export interface PairingOptions {
  /** Permissions for the device that pairs with the code. Defaults to ["view"]. */
  grant?: string[];
  /** Seconds the code stays valid. Defaults to 300. */
  ttlSecs?: number;
}

export interface HostEvent {
  kind: 'paired' | 'session-opened' | 'session-closed' | 'refused';
  message: string;
}

/** `move`: x,y are 0..1 of the display. `delta`: dx,dy in pixels. `scroll`: lines, positive is down or right. */
export type InputEvent =
  | { kind: 'move'; x: number; y: number }
  | { kind: 'delta'; dx: number; dy: number }
  | { kind: 'button'; button: 'left' | 'right' | 'middle' | 'back' | 'forward'; down: boolean }
  | { kind: 'scroll'; dx?: number; dy?: number }
  | { kind: 'key'; code: string; down: boolean }
  | { kind: 'text'; text: string };

export interface ParsedLink {
  address: string;
  code: string;
  name?: string;
}

/** The link a host shows as a QR code: tether://pair?addr=...&code=...&name=... */
export function pairingLink(address: string, code: string, name?: string | null): string;
/** Reads a pairing link. Throws a readable error if it is not one. */
export function parsePairingLink(link: string): ParsedLink;

export type PermissionName = 'view' | 'input' | 'media' | 'power' | 'files';

/** This machine's identity and trust store. */
export class Device {
  /** `home` overrides the data directory. */
  constructor(home?: string | null, name?: string | null);
  readonly name: string;
  readonly fingerprint: string;
  readonly home: string;
  peers(): PeerSummary[];
  grant(query: string, permissions: string[]): PeerSummary[];
  revoke(query: string, permissions: string[]): PeerSummary[];
  /** Returns how many devices were unpaired. */
  remove(query: string): number;
  pair(address: string, code: string): Promise<PeerSummary>;
  connect(address: string): Promise<Connection>;
  /** `video` lets permitted devices watch a stream: "test-pattern", "screen" or "screen:<id>" (see listDisplays). */
  /** `input: true` lets devices with the "input" permission control this computer. Off by default. */
  listen(
    bind: string,
    pairing?: PairingOptions | null,
    video?: string | null,
    input?: boolean | null,
  ): Promise<HostHandle>;
}

export class Connection {
  readonly peer: PeerSummary;
  /** What the host currently allows this device to do. Refreshed by ping(). */
  readonly permissions: string[];
  /** Round trip time in milliseconds. */
  ping(): Promise<number>;
  /** Sends one mouse or keyboard event. Refusals come back later through takeNotices(). */
  sendInput(event: InputEvent): Promise<void>;
  /** Messages from the host for the person to read. Collected during ping(). */
  takeNotices(): Promise<string[]>;
  /** Sends a WebRTC offer, resolves with the host's answer. Needs the "view" permission. */
  requestVideo(offer: string): Promise<string>;
  close(): Promise<void>;
}

export class HostHandle {
  /** The address actually bound, for example "127.0.0.1:7420". */
  readonly address: string;
  readonly pairingCode: string | null;
  /** Returns and clears what happened since the last call. */
  drainEvents(): HostEvent[];
  stop(): void;
}

export interface Display {
  id: number;
  name: string;
  width: number;
  height: number;
  primary: boolean;
}

/** The displays this computer can share, primary first. */
export function listDisplays(): Display[];
