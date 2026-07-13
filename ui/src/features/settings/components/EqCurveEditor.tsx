import type { PointerEvent as ReactPointerEvent } from 'react';
import { buildEqCurve, EQ_PLOT_HEIGHT, EQ_PLOT_PAD_X, EQ_PLOT_WIDTH } from '../settingsModel';

export type EqCurve = ReturnType<typeof buildEqCurve>;

export function EqCurveEditor({
  dragEqBand,
  draggingEqBand,
  eqCurve,
  startEqBandDrag,
  stopEqBandDrag
}: {
  dragEqBand: (event: ReactPointerEvent<SVGSVGElement>) => void;
  draggingEqBand: number | null;
  eqCurve: EqCurve;
  startEqBandDrag: (index: number, event: ReactPointerEvent<SVGCircleElement>) => void;
  stopEqBandDrag: (event: ReactPointerEvent<SVGSVGElement>) => void;
}) {
  return (
    <div className="eq-curve-wrap">
      <svg
        id="eq-curve"
        viewBox={`0 0 ${EQ_PLOT_WIDTH} ${EQ_PLOT_HEIGHT}`}
        onPointerMove={dragEqBand}
        onPointerUp={stopEqBandDrag}
        onPointerCancel={stopEqBandDrag}
      >
        <defs>
          <linearGradient id="eq-curve-grad" x1="0%" y1="0%" x2="0%" y2="100%">
            <stop offset="0%" stopColor="currentColor" stopOpacity="0.10" />
            <stop offset="100%" stopColor="currentColor" stopOpacity="0" />
          </linearGradient>
        </defs>
        <g className="eq-grid">
          {eqCurve.verticalGrid.map((line) => (
            <line
              x1={line.x}
              y1="0"
              x2={line.x}
              y2={EQ_PLOT_HEIGHT}
              className={line.major ? 'eq-grid-major' : undefined}
              key={`v-${line.freq}`}
            />
          ))}
          {eqCurve.horizontalGrid.map((line) => (
            <line
              x1={EQ_PLOT_PAD_X}
              y1={line.y}
              x2={EQ_PLOT_WIDTH - EQ_PLOT_PAD_X}
              y2={line.y}
              className={line.db === 0 ? 'eq-grid-zero' : undefined}
              key={`h-${line.db}`}
            />
          ))}
        </g>
        <path id="eq-curve-fill" fill="url(#eq-curve-grad)" d={eqCurve.fillPath}></path>
        <path
          id="eq-curve-path"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.5"
          d={eqCurve.path}
        ></path>
        <g className="eq-band-markers">
          {eqCurve.markers.map((marker) => (
            <g
              className={`eq-marker-group${marker.enabled ? '' : ' disabled'}${draggingEqBand === marker.index ? ' dragging' : ''}`}
              data-idx={marker.index}
              key={marker.index}
            >
              <circle className="eq-marker-dot" cx={marker.x} cy={marker.y} r="3" />
              <text className="eq-marker-label" x={marker.x} y={marker.y - 10} textAnchor="middle">
                {marker.index + 1}
              </text>
              <circle
                className="eq-marker-hit"
                cx={marker.x}
                cy={marker.y}
                r="14"
                style={{ fill: 'transparent', cursor: marker.enabled ? 'grab' : 'pointer' }}
                onPointerDown={(event) => startEqBandDrag(marker.index, event)}
              />
            </g>
          ))}
        </g>
      </svg>
    </div>
  );
}
