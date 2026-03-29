import js from '@eslint/js'
import globals from 'globals'
import reactHooks from 'eslint-plugin-react-hooks'
import reactRefresh from 'eslint-plugin-react-refresh'
import tseslint from 'typescript-eslint'

export default tseslint.config(
  // Ignore dist and node_modules directories
  { ignores: ['dist/', 'node_modules/'] },
  
  // General rules for all TypeScript files
  {
    files: ['**/*.{ts,tsx}'],
    extends: [
      js.configs.recommended,
      ...tseslint.configs.recommended,
    ],
    languageOptions: {
      ecmaVersion: 2020,
      globals: globals.browser,
    },
    rules: {
      // General best practices
      'no-console': 'warn',
      'no-debugger': 'error',
      
      // TypeScript specific rules
      '@typescript-eslint/no-unused-vars': ['error', {
        argsIgnorePattern: '^_',
        varsIgnorePattern: '^_'
      }],
      '@typescript-eslint/explicit-function-return-type': 'off',
      '@typescript-eslint/no-explicit-any': 'warn',
      '@typescript-eslint/consistent-type-imports': 'error',
    },
  },
  
  // Additional rules for React files
  {
    files: ['src/**/*.{ts,tsx}'],
    extends: [
      reactHooks.configs['recommended-latest'],
      reactRefresh.configs.vite,
    ],
    rules: {
      'react-refresh/only-export-components': 'warn',
    },
  },
  
  // Stricter rules for SDK files
  {
    files: ['src/sdk/**/*.{ts,tsx}'],
    rules: {
      // Enforce stricter checking for SDK code
      '@typescript-eslint/no-explicit-any': 'warn',
      
      // Better code quality for SDK
      '@typescript-eslint/explicit-function-return-type': 'warn',
      '@typescript-eslint/explicit-module-boundary-types': 'warn',
      
      // Additional rules for better SDK quality
      'no-console': ['warn', { allow: ['warn', 'error'] }],
      '@typescript-eslint/no-unused-vars': ['error', {
        argsIgnorePattern: '^_',
        varsIgnorePattern: '^_',
        caughtErrorsIgnorePattern: '^_'
      }],
      '@typescript-eslint/no-empty-function': 'warn',
      'no-throw-literal': 'error',
    },
  },
  
  // Configuration files
  {
    files: ['**/*.config.{js,ts}', '**/*.conf.{js,ts}'],
    languageOptions: {
      globals: globals.node,
    },
    rules: {
      '@typescript-eslint/no-var-requires': 'off',
    },
  },
)
