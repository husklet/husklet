import base from '../../eslint.config.js';

export default [
  ...base,
  {
    files: ['src/**/*.{ts,tsx}'],
    rules: {
      // Husklet targets React 18 and does not enable the React Compiler.
      'react-hooks/refs': 'off',
      'react-hooks/set-state-in-effect': 'off',
    },
  },
];
