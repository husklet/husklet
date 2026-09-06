import eslint from '@eslint/js';
import prettier from 'eslint-config-prettier';
import reactHooks from 'eslint-plugin-react-hooks';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  {
    ignores: ['**/dist/**', '**/node_modules/**', 'extensions/storybook/src/catalogue.json'],
  },
  eslint.configs.recommended,
  ...tseslint.configs.recommended,
  reactHooks.configs.flat.recommended,
  {
    files: [
      'extensions/*/src/**/*.{ts,tsx}',
      'extensions/*/examples/**/*.{ts,tsx}',
      'extensions/*/vite.config.ts',
    ],
    rules: {
      curly: ['error', 'all'],
      eqeqeq: ['error', 'always', { null: 'ignore' }],
      'no-else-return': 'error',
      'no-lonely-if': 'error',
      'no-unneeded-ternary': 'error',
      'object-shorthand': 'error',
      // Extension entrypoints intentionally assign render handles after callbacks
      // that close over them have been constructed.
      'prefer-const': 'off',
      'prefer-template': 'error',
      // These React Compiler rules reject ordinary React 18 ref and synchronization
      // patterns. The extension runtime does not use the compiler.
      'react-hooks/immutability': 'off',
      'react-hooks/refs': 'off',
      'react-hooks/set-state-in-effect': 'off',
      // Protocol validation deliberately detects ASCII control bytes.
      'no-control-regex': 'off',
    },
  },
  prettier,
);
