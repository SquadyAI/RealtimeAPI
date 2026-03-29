// Type definitions for @evan/opus/wasm
// Project: https://github.com/evanwashere/opus
// Definitions by: KiloCode

declare module '@evan/wasm/target/opus/deno.js' {
  /**
   * Opus WebAssembly module
   */
  export class Encoder {
    /**
     * Create a new Opus encoder
     */
    constructor(options: {
      /** Number of channels (default: 2) */
      channels?: number;
      /** Sample rate in Hz (default: 48000) */
      sample_rate?: number;
      /** Application type (default: 'audio') */
      application?: 'voip' | 'audio' | 'restricted_lowdelay';
    });

    /**
     * Encode raw PCM data into Opus packets
     * @param buffer - Raw PCM data
     * @returns Encoded Opus packet
     */
    encode(buffer: ArrayBufferView): Uint8Array;

    /**
     * Control the encoder
     * @param cmd - Control command
     * @param arg - Optional argument
     * @returns Result when getting, undefined when setting
     */
    ctl(cmd: number, arg?: number): number | void;

    /**
     * Drop the encoder and free resources
     */
    drop(): void;
  }

  export class Decoder {
    /**
     * Create a new Opus decoder
     */
    constructor(options: {
      /** Number of channels (default: 2) */
      channels?: number;
      /** Sample rate in Hz (default: 48000) */
      sample_rate?: number;
    });

    /**
     * Decode an Opus packet into raw PCM data
     * @param buffer - Encoded Opus packet
     * @returns Decoded raw PCM data
     */
    decode(buffer: ArrayBufferView): Uint8Array;

    /**
     * Control the decoder
     * @param cmd - Control command
     * @param arg - Optional argument
     * @returns Result when getting, undefined when setting
     */
    ctl(cmd: number, arg?: number): number | void;

    /**
     * Drop the decoder and free resources
     */
    drop(): void;
  }
}