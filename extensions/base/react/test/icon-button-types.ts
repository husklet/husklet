import type { IconButtonProps } from '../dist/index.js';

const valid: IconButtonProps = { icon: 'view-refresh-symbolic', label: 'Refresh' };
void valid;

// @ts-expect-error IconButton requires an accessible action name.
const missingLabel: IconButtonProps = { icon: 'view-refresh-symbolic' };
void missingLabel;

// @ts-expect-error IconButton requires visible icon content.
const missingIcon: IconButtonProps = { label: 'Refresh' };
void missingIcon;
