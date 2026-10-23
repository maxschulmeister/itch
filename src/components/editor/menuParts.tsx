import { forwardRef, type ComponentPropsWithoutRef, type ComponentType } from "react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { ChevronRightIcon } from "../icons";
import { cn } from "../../lib/utils";

type Icon = ComponentType<{ className?: string }>;

/**
 * Marks the portaled menus of the selection toolbar and the AI menu, so that
 * a focus move into them does not close the toolbar.
 */
export const MENU_LAYER_ATTR = "data-floating-menu-layer";

export const isInMenuLayer = (node: Node | null) =>
  node instanceof Element && !!node.closest(`[${MENU_LAYER_ATTR}]`);

const FOCUS_RING =
  "outline-none focus-visible:outline-solid focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ai";

/** A 32px button of the floating toolbars and the AI menu. */
export const MENU_BUTTON = cn(
  "inline-flex items-center justify-center shrink-0 h-8 min-w-8 p-2 gap-0.5 rounded-xl",
  "text-sm font-medium text-menu-fg cursor-pointer select-none",
  "transition-[background-color,color] duration-200 ease-menu",
  "hover:bg-menu-hover hover:text-text",
  "aria-pressed:bg-bg-muted aria-pressed:text-text",
  // A dropdown button is open (aria-expanded: a tooltip sets data-state too).
  "aria-expanded:bg-bg-muted aria-expanded:text-text",
  "disabled:text-text/40 disabled:bg-transparent disabled:cursor-default",
  FOCUS_RING,
);

/** The primary (AI) variant of `MENU_BUTTON`. */
export const MENU_BUTTON_PRIMARY = cn(
  MENU_BUTTON,
  "bg-ai text-text-inverse hover:bg-ai-hover hover:text-text-inverse",
  "disabled:bg-bg-muted disabled:text-text/40",
  "focus-visible:outline-offset-2",
);

/** The text of a button with an icon. */
export const MENU_BUTTON_TEXT = "px-0.5 leading-6 whitespace-nowrap";

export const MENU_ICON = "w-4 h-4 shrink-0";

/** The box of the floating toolbars. */
export const TOOLBAR_BOX =
  "flex items-center gap-1 p-[3px] bg-bg border border-border rounded-[15px] shadow-menu";

/** The box of the dropdown menus and the AI menu cards. */
export const CARD_BOX = "bg-bg-elevated border border-border shadow-menu";

const ITEM = cn(
  "flex w-full items-center h-8 p-2 gap-1 rounded-xl",
  "text-sm font-medium text-menu-fg text-left cursor-pointer select-none outline-none",
  "transition-[background-color,color] duration-200 ease-menu",
  "data-highlighted:bg-menu-hover data-highlighted:text-text",
  "data-[state=open]:bg-menu-hover data-[state=open]:text-text",
  "data-disabled:text-text/40 data-disabled:cursor-default",
);

const CONTENT = cn(
  "z-[10000] min-w-0 p-1.5 rounded-[18px]",
  CARD_BOX,
  "border-transparent overflow-y-auto overscroll-contain",
  "max-h-(--radix-dropdown-menu-content-available-height)",
  "origin-(--radix-dropdown-menu-content-transform-origin)",
  "animate-menu-in motion-reduce:animate-none",
);

/** Dropdown content in a portal, with the menu style. */
type PortalProps = {
  /** The portal target. Default: the document body. */
  container?: HTMLElement | null;
};

export const MenuContent = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof DropdownMenu.Content> & PortalProps
>(function MenuContent({ className, sideOffset = 4, container, ...props }, ref) {
  return (
    <DropdownMenu.Portal container={container}>
      <DropdownMenu.Content
        ref={ref}
        sideOffset={sideOffset}
        collisionPadding={8}
        data-floating-menu-layer=""
        className={cn(CONTENT, className)}
        {...props}
      />
    </DropdownMenu.Portal>
  );
});

export function MenuSubContent({
  className,
  container,
  ...props
}: ComponentPropsWithoutRef<typeof DropdownMenu.SubContent> & PortalProps) {
  return (
    <DropdownMenu.Portal container={container}>
      <DropdownMenu.SubContent
        sideOffset={-6}
        collisionPadding={8}
        data-floating-menu-layer=""
        className={cn(
          CONTENT,
          "p-1 max-h-(--radix-dropdown-menu-content-available-height)",
          className,
        )}
        {...props}
      />
    </DropdownMenu.Portal>
  );
}

interface ItemProps {
  icon?: Icon;
  label: string;
  /** Colors the icon (the current block type). */
  iconClassName?: string;
}

function ItemBody({ icon: ItemIcon, label, iconClassName }: ItemProps) {
  return (
    <>
      {ItemIcon && <ItemIcon className={cn(MENU_ICON, iconClassName)} />}
      <span className={cn(MENU_BUTTON_TEXT, "flex-1 truncate")}>{label}</span>
    </>
  );
}

export function MenuItem({
  icon,
  label,
  iconClassName,
  className,
  ...props
}: ItemProps & ComponentPropsWithoutRef<typeof DropdownMenu.Item>) {
  return (
    <DropdownMenu.Item className={cn(ITEM, className)} {...props}>
      <ItemBody icon={icon} label={label} iconClassName={iconClassName} />
    </DropdownMenu.Item>
  );
}

export function MenuRadioItem({
  icon,
  label,
  className,
  ...props
}: ItemProps & ComponentPropsWithoutRef<typeof DropdownMenu.RadioItem>) {
  return (
    <DropdownMenu.RadioItem
      className={cn(
        ITEM,
        "data-[state=checked]:bg-bg-muted data-[state=checked]:text-text data-[state=checked]:[&>svg]:text-ai",
        className,
      )}
      {...props}
    >
      <ItemBody icon={icon} label={label} />
    </DropdownMenu.RadioItem>
  );
}

export function MenuSubTrigger({
  icon,
  label,
  className,
  ...props
}: ItemProps & ComponentPropsWithoutRef<typeof DropdownMenu.SubTrigger>) {
  return (
    <DropdownMenu.SubTrigger className={cn(ITEM, className)} {...props}>
      <ItemBody icon={icon} label={label} />
      <ChevronRightIcon className={MENU_ICON} />
    </DropdownMenu.SubTrigger>
  );
}

export function MenuSeparator() {
  return <DropdownMenu.Separator className="h-px my-2 bg-bg-emphasis" />;
}

export function MenuLabel({ children }: { children: string }) {
  return (
    <DropdownMenu.Label className="pt-3 px-2 pb-1 text-2xs leading-[15px] font-semibold text-text capitalize">
      {children}
    </DropdownMenu.Label>
  );
}
