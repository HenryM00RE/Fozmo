import { describe, expect, it } from 'vitest';
import { buildCapabilities, zoneAvailableForCapabilities } from './capabilities';

describe('zoneAvailableForCapabilities', () => {
  it('keeps remote-agent ASIO outputs visible when the core has no ASIO build', () => {
    const capabilities = buildCapabilities({ capabilities: { asio: false } });

    expect(
      zoneAvailableForCapabilities(
        {
          id: 'agent-windows-asio-brooklyn',
          name: 'HENRYSPC - Brooklyn DAC+',
          protocol: 'remote_agent',
          backend: 'asio',
          device_name: 'ASIO: Brooklyn DAC+'
        },
        capabilities
      )
    ).toBe(true);
  });

  it('still hides a local ASIO output when the core has no ASIO build', () => {
    const capabilities = buildCapabilities({ capabilities: { asio: false } });

    expect(
      zoneAvailableForCapabilities(
        {
          id: 'local-asio-brooklyn',
          name: 'Brooklyn DAC+',
          protocol: 'asio_output',
          backend: 'asio',
          device_name: 'ASIO: Brooklyn DAC+'
        },
        capabilities
      )
    ).toBe(false);
  });
});
