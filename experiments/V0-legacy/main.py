"""云农之窗 FastAPI 入口：只保留应用初始化与路由端点。"""

from dotenv import load_dotenv
from fastapi import BackgroundTasks, FastAPI, File, Form, UploadFile
from fastapi.responses import FileResponse, RedirectResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

from services import ai_service, campus_api, memory_service

load_dotenv()

app = FastAPI()
app.mount("/static", StaticFiles(directory="static", html=True), name="static")
app.include_router(campus_api.router)


@app.get("/", include_in_schema=False)
async def root():
    """根路径重定向到静态首页，这样直接分享公网域名就能打开页面。"""
    return RedirectResponse(url="/static/", status_code=307)


@app.get("/favicon.ico", include_in_schema=False)
async def favicon():
    return FileResponse("static/favicon.ico")


@app.get("/manifest.webmanifest", include_in_schema=False)
async def manifest():
    return FileResponse(
        "static/manifest.webmanifest", media_type="application/manifest+json"
    )


class ChatRequest(BaseModel):
    message: str


class ProfileDeleteRequest(BaseModel):
    field: str
    value: str = ""


@app.get("/api/test")
async def test():
    return {"reply": "你好，我是云农之窗"}


@app.get("/api/profile")
async def get_profile():
    return memory_service.load_profile()


@app.post("/api/profile/delete")
async def delete_profile_field(req: ProfileDeleteRequest):
    return memory_service.delete_profile_field(req.field, req.value)


@app.post("/api/profile/reset")
async def reset_profile():
    return {"ok": True, "profile": memory_service.reset_profile()}


@app.post("/api/upload_file")
async def upload_file(file: UploadFile = File(...), question: str = Form("")):
    try:
        content = await file.read()
        return ai_service.handle_document_upload(
            file.filename or "upload", content, file.content_type, question
        )
    except Exception as e:
        return {"ok": False, "error": str(e)}


@app.get("/api/file_state")
async def get_file_state():
    return memory_service.load_file_state()


@app.post("/api/clear")
async def clear_memory():
    """开始新对话：清空 memory.json 中的聊天记忆。"""
    memory_service.clear_memory()
    return {"ok": True}


@app.post("/api/upload_file/clear")
async def clear_file_state():
    memory_service.save_file_state({})
    return {"ok": True}


@app.post("/api/chat")
async def chat(req: ChatRequest, background_tasks: BackgroundTasks):
    background_tasks.add_task(ai_service.update_profile_from_message, req.message)
    return {"reply": ai_service.chat_reply(req.message)}


@app.post("/api/vision")
async def vision(file: UploadFile = File(...)):
    try:
        content = await file.read()
        return {"reply": ai_service.describe_image(content, file.content_type or "image/jpeg")}
    except Exception as e:
        return {"reply": f"识图失败：{e}"}
