import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    exclude: ['e2e/**', 'node_modules/**', 'dist/**'],
    setupFiles: ['./src/test/setup.ts'],
    coverage: {
      provider: 'v8',
      reporter: ['text', 'json-summary', 'html'],
      reportsDirectory: './coverage',
      include: ['src/**/*.{ts,tsx}'],
      exclude: [
        'src/**/*.test.{ts,tsx}',
        'src/shared/generated/**',
        'src/test/**',
        'src/main.tsx',
        'src/vite-env.d.ts'
      ],
      thresholds: {
        branches: 14,
        functions: 12,
        lines: 14,
        statements: 14
      }
    }
  }
});
