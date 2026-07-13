export type ToolbarAction = {
  label: string;
  onClick: () => void | Promise<void>;
  title?: string;
};
