import React from "react";
import { AppInfo } from "../types/Apps";
import { appId } from "../utils/app";
import { useNavigate } from "react-router-dom";
import classNames from "classnames";
import { APP_DETAILS_PATH } from "../constants/path";
import ColorDot from "./ColorDot";
import { isMobileCheck } from "../utils/dimensions";

interface AppHeaderProps extends React.HTMLAttributes<HTMLDivElement> {
  app: AppInfo;
  size?: "small" | "medium" | "large";
}

export default function AppHeader({
  app,
  size = "medium",
  ...props
}: AppHeaderProps) {
  const navigate = useNavigate()
  const isMobile = isMobileCheck()

  return (
    <div
      {...props}
      className={classNames('flex w-full justify-content-start', size, props.className, { 'cursor-pointer': size !== 'large' })}
      onClick={() => navigate(`/${APP_DETAILS_PATH}/${appId(app)}`)}
    >
      {app.metadata?.image
        ? <img
          src={app.metadata.image}
          alt="app icon"
          className={classNames('mr-2', { 'h-32 rounded-md': size === 'large', 'h-12 rounded': size !== 'large' })}
        />
        : <ColorDot
          num={app.metadata_hash}
          dotSize={size}
          className={classNames('mr-2')}
        />}
      <div className={classNames("flex flex-col", { 'gap-2 max-w-3/4': isMobile })}>
        <div
          className={classNames("whitespace-nowrap overflow-hidden text-ellipsis", { 'text-3xl': size === 'large', })}
        >
          {app.metadata?.name || appId(app)}
        </div>
        {app.metadata?.description && size !== "large" && (
          <div className="whitespace-nowrap overflow-hidden text-ellipsis">
            {app.metadata?.description?.slice(0, 100)}
          </div>
        )}
      </div>
    </div>
  );
}
