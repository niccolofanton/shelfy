// Conventional Commits — matches the feat/fix/chore/docs/refactor/test scopes
// already used throughout the history (see `git log`).
export default {
  extends: ['@commitlint/config-conventional'],
  rules: {
    // Allow the slightly longer subjects this project already uses.
    'header-max-length': [2, 'always', 100],
    // Body/footer wrapping is not enforced (release notes paste freely).
    'body-max-line-length': [0, 'always', Infinity],
  },
};
