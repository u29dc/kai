export default {
	extends: ['@commitlint/config-conventional'],
	rules: {
		'type-enum': [2, 'always', ['feat', 'fix', 'refactor', 'docs', 'style', 'chore', 'test']],
		'type-empty': [2, 'never'],
		'scope-enum': [2, 'always', ['sdk', 'cli', 'docs', 'config', 'deps', 'infra']],
		'scope-empty': [2, 'never'],
		'subject-empty': [2, 'never'],
		'subject-case': [2, 'always', 'lower-case'],
		'subject-full-stop': [2, 'never', '.'],
		'header-max-length': [2, 'always', 100],
		'body-max-line-length': [2, 'always', 100],
	},
};
