from datetime import datetime

import heap_profiler
from flask import Flask, Response

app = Flask(__name__)


# golang style pprof server
@app.route("/debug/pprof/heap")
def index() -> Response:
    filename = f"heap-{datetime.now().isoformat()}.pb.gz"
    return Response(
        heap_profiler.get_pprof_data(compress=True),
        mimetype="application/octet-stream",
        headers={"Content-Disposition": f"attachment; filename={filename}"},
    )


if __name__ == "__main__":
    heap_profiler.start_tracing()
    app.run(port=6066)
